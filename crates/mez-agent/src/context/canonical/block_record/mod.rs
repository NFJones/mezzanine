//! Durable block occurrence identity and owned, unmeasured input estimates.
//!
//! Ordering anchors, execution groups and replay membership are independent of
//! an occurrence. Immutable canonical owners retain this record, so rebuilding
//! a read projection never hashes or recounts unchanged text. Persisted records
//! are descriptive input: restoration validates their rendered association and
//! recomputes stale estimates before adopting them. Neither IDs nor costs grant
//! source, instruction, provider or execution authority.
//! Named stable snapshots select records before slot construction; current
//! producer blocks still supply all instructions and source fingerprints.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, StableContextBlock,
    StableContextSlotId, StableContextSourceFingerprint, model_context_block_header,
};

const RECORD_VERSION: u32 = 1;
const UTF8_QUARTER_METHOD: u32 = 1;

/// Immutable occurrence identity, unrelated to content or local event order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContextBlockOccurrenceId([u8; 16]);

impl ContextBlockOccurrenceId {
    /// Allocates a fresh occurrence. Cloning preserves it; equal authored bytes
    /// at another commit receive a different identity.
    pub fn allocate() -> Self {
        Self(*uuid::Uuid::new_v4().as_bytes())
    }

    /// Establishes an explicit legacy import identity from an actual archived
    /// row and its structural projection ordinal. This is an import boundary,
    /// not evidence that old records already persisted an occurrence ID.
    /// Empty namespaces, zero row positions and invalid namespaces are refused.
    pub fn for_legacy_archive_row(
        conversation: &str,
        row_sequence: u64,
        projection_ordinal: u32,
    ) -> AgentContextResult<Self> {
        if conversation.is_empty()
            || conversation.trim() != conversation
            || conversation.chars().any(char::is_control)
            || row_sequence == 0
        {
            return Err(invalid("invalid legacy archive occurrence"));
        }
        let mut hash = Sha256::new();
        hash.update(b"mez-context-legacy-archive-occurrence-v1\0");
        hash.update((conversation.len() as u64).to_le_bytes());
        hash.update(conversation.as_bytes());
        hash.update(row_sequence.to_le_bytes());
        hash.update(projection_ordinal.to_le_bytes());
        let digest = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        // Keep the UUID namespace distinct from newly allocated v4 occurrences.
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for ContextBlockOccurrenceId {
    type Error = AgentContextError;

    /// Rejects nil and noncanonical spellings rather than creating aliases.
    fn try_from(value: String) -> AgentContextResult<Self> {
        let uuid = uuid::Uuid::parse_str(&value).map_err(|_| invalid("invalid occurrence ID"))?;
        if uuid.is_nil() || uuid.to_string() != value {
            return Err(invalid("invalid occurrence ID"));
        }
        Ok(Self(*uuid.as_bytes()))
    }
}

impl From<ContextBlockOccurrenceId> for String {
    /// Encodes the canonical lowercase, hyphenated persistent spelling.
    fn from(value: ContextBlockOccurrenceId) -> Self {
        uuid::Uuid::from_bytes(value.0).to_string()
    }
}

/// Versioned accounting associated with one exact rendered block revision.
/// Fields stay private; deserialization alone does not validate a cached cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBlockRecord {
    format_version: u32,
    occurrence_id: ContextBlockOccurrenceId,
    counting_method: u32,
    rendered_sha256: [u8; 32],
    framed_input_tokens: usize,
    native_input_tokens: usize,
}

/// A stable slot's descriptive persistent accounting, without its instructions.
/// The active owner supplies and validates the actual source on restoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StableContextBlockRecord {
    slot_id: String,
    source_fingerprint: String,
    record: ContextBlockRecord,
}

impl StableContextBlockRecord {
    /// Returns the immutable occurrence carried by this descriptive snapshot.
    /// It grants no instruction or source authority.
    pub fn occurrence_id(&self) -> ContextBlockOccurrenceId {
        self.record.occurrence_id()
    }

    /// Returns the explicit producer slot, never an inferred text match.
    pub fn slot_id(&self) -> &str {
        &self.slot_id
    }

    /// Validates only record/slot framing; actual live source association still
    /// requires canonical restoration. This grants no instruction authority.
    pub fn validate_format(&self) -> AgentContextResult<()> {
        StableContextSlotId::new(self.slot_id.clone())?;
        StableContextSourceFingerprint::new(self.source_fingerprint.clone())?;
        self.record.validate_format()
    }
}

impl ContextBlockRecord {
    /// Captures a newly committed occurrence and its current unmeasured costs.
    /// Refuses rendered length overflow without changing canonical storage.
    pub fn allocate(block: &ContextBlock) -> AgentContextResult<Self> {
        Self::for_occurrence(ContextBlockOccurrenceId::allocate(), block)
    }

    /// Captures a known immutable occurrence at an explicit producer/import
    /// boundary. The caller still owns semantic and causal validation.
    pub fn for_occurrence(
        occurrence_id: ContextBlockOccurrenceId,
        block: &ContextBlock,
    ) -> AgentContextResult<Self> {
        #[cfg(test)]
        ESTIMATE_COMPUTATIONS.with(|count| count.set(count.get() + 1));
        let header = model_context_block_header(block);
        let bytes = header
            .len()
            .checked_add(block.content.len())
            .ok_or_else(|| invalid("rendered length overflow"))?;
        Ok(Self {
            format_version: RECORD_VERSION,
            occurrence_id,
            counting_method: UTF8_QUARTER_METHOD,
            rendered_sha256: rendered_digest(&header, &block.content),
            framed_input_tokens: bytes.div_ceil(4),
            native_input_tokens: block.content.len().div_ceil(4),
        })
    }

    /// Returns immutable origin, never an ordering anchor or a live capability.
    pub fn occurrence_id(self) -> ContextBlockOccurrenceId {
        self.occurrence_id
    }

    /// Cached rendered revision of this actual occurrence. Canonical admission
    /// or restoration validates its source; reading it never rehashes payloads.
    pub(super) fn rendered_fingerprint(self) -> [u8; 32] {
        self.rendered_sha256
    }

    /// Returns the UTF-8-quarter estimate including the ordinary block header.
    /// Provider wire/schema overhead and reported billing remain separate.
    pub fn estimated_input_tokens(self) -> usize {
        self.framed_input_tokens
    }

    /// Returns the content-only estimate used by the existing native projection.
    /// Visibility and complete-group neutral dedup remain provider policy.
    pub fn estimated_native_input_tokens(self) -> usize {
        self.native_input_tokens
    }

    /// Preserves occurrence identity while revising its actual rendered bytes.
    /// Only explicit content/rendering mutation should call this method.
    pub fn revised(self, block: &ContextBlock) -> AgentContextResult<Self> {
        Self::for_occurrence(self.occurrence_id, block)
    }

    /// Rejects unsupported record schemas before a semantic source is used.
    /// Cost/revision fields remain unchecked until restoration against source.
    pub fn validate_format(self) -> AgentContextResult<()> {
        if self.format_version != RECORD_VERSION {
            return Err(invalid("unsupported record format"));
        }
        Ok(())
    }

    /// Validates a persisted cache once at import. Unknown record formats refuse
    /// reinterpretation; stale method, revision or authored costs recompute from
    /// the actual supplied block, preserving its occurrence. This never trusts
    /// estimates as billing or uses them to grant source authority.
    pub fn restored(self, block: &ContextBlock) -> AgentContextResult<Self> {
        self.validate_format()?;
        let header = model_context_block_header(block);
        let bytes = header
            .len()
            .checked_add(block.content.len())
            .ok_or_else(|| invalid("rendered length overflow"))?;
        if self.counting_method == UTF8_QUARTER_METHOD
            && self.rendered_sha256 == rendered_digest(&header, &block.content)
            && self.framed_input_tokens == bytes.div_ceil(4)
            && self.native_input_tokens == block.content.len().div_ceil(4)
        {
            Ok(self)
        } else {
            Self::for_occurrence(self.occurrence_id, block)
        }
    }
}

impl AgentContext {
    /// Captures stable accounting alongside actual producer slot fingerprints.
    /// Instruction bytes remain outside this non-model-visible snapshot.
    pub fn stable_block_records(&self) -> Vec<StableContextBlockRecord> {
        self.stable_slots
            .iter()
            .map(|slot| StableContextBlockRecord {
                slot_id: slot.slot_id.as_str().to_owned(),
                source_fingerprint: slot.source_fingerprint.as_str().to_owned(),
                record: slot.record,
            })
            .collect()
    }

    /// Imports an ordered source with structurally associated persistent records.
    /// Indices must be unique, increasing and in bounds. Missing records denote
    /// an explicit unaccounted import; valid records are reused at construction,
    /// stale costs recompute, and duplicate occurrences refuse the whole owner.
    /// Named stable snapshots are selected before construction against the live
    /// structural slot; their saved fingerprints never replace current source.
    /// Removed slots remain absent. Overlapping indexed/named records must agree.
    /// Source, retention and execution validation remains canonical policy.
    pub fn import_durable_blocks_with_records(
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        stable_records: &[StableContextBlockRecord],
    ) -> AgentContextResult<Self> {
        Self::import_durable_blocks_with_stable_sources(blocks, records, stable_records, &[])
    }

    /// Imports current producer-keyed stable slots alongside indexed history.
    /// Keys come from the actual producer, independently of prefix position or
    /// text. Legacy positional snapshots migrate only at the same provable
    /// structural slot; missing provenance never selects a nearest text match.
    /// Duplicate, out-of-prefix or conflicting input associations fail adoption.
    pub fn import_durable_blocks_with_stable_sources(
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        stable_records: &[StableContextBlockRecord],
        stable_sources: &[(usize, StableContextSlotId)],
    ) -> AgentContextResult<Self> {
        validate_record_indices(blocks.len(), records)?;
        let mut context = Self::empty();
        context.initialize_typed_storage_with_records(
            blocks,
            records,
            stable_records,
            stable_sources,
        )?;
        context.validate_durable()?;
        Ok(context)
    }

    /// Replaces an explicit historical prefix while reusing persistent records.
    /// Retained live events keep their own origin and order. Invalid records,
    /// duplicate occurrences and an exhausted ordering interval are atomic
    /// failures; a bare block replacement cannot silently invent restored IDs.
    pub fn replace_imported_history_prefix_through_sequence_with_records(
        &mut self,
        sequence_high_water: u64,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
    ) -> AgentContextResult<usize> {
        validate_record_indices(blocks.len(), records)?;
        self.replace_imported_history_prefix_events(
            |event| event.sequence().get() <= sequence_high_water,
            blocks,
            records,
        )
    }

    /// Returns snapshots aligned with the exact current model-block projection.
    /// These carry no source text, membership, causal or instruction authority.
    pub fn block_records(&self) -> Vec<(usize, ContextBlockRecord)> {
        self.block_metadata
            .iter()
            .enumerate()
            .map(|(index, metadata)| (index, metadata.record()))
            .collect()
    }

    /// Restores explicitly associated records without changing any source bytes
    /// or semantic ownership. Failure, including aliasing an existing occurrence,
    /// leaves the canonical owner unchanged. Imported cost fields never grant
    /// permission to change content, order or execution-group attribution.
    pub fn restore_block_records(
        &mut self,
        records: &[(usize, ContextBlockRecord)],
    ) -> AgentContextResult<()> {
        validate_record_indices(self.blocks.len(), records)?;
        let mut candidate = self.clone();
        for &(index, record) in records {
            if let Some(slot) = candidate.stable_slots.get_mut(index) {
                slot.record = record.restored(&slot.block)?;
            } else {
                let event = &mut candidate.chronology[index - candidate.stable_slots.len()];
                event.record = record.restored(&event.block)?;
            }
        }
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        *self = candidate;
        Ok(())
    }

    /// Keeps a refreshed stable slot's origin while revising only changed bytes.
    /// Slot matching is an explicit producer identity, never a text heuristic.
    pub(super) fn preserve_stable_record_origins(
        &self,
        slots: &mut [StableContextBlock],
    ) -> AgentContextResult<()> {
        for slot in slots {
            if let Some(previous) = self
                .stable_slots
                .iter()
                .find(|old| old.slot_id == slot.slot_id)
            {
                slot.record = if previous.block == slot.block {
                    previous.record
                } else {
                    previous.record.revised(&slot.block)?
                };
            }
        }
        Ok(())
    }
}

/// Validates all named snapshots before constructing any estimates, including
/// snapshots whose slots were removed. Only current live slots select records.
pub(super) fn validated_stable_records(
    records: &[StableContextBlockRecord],
) -> AgentContextResult<std::collections::BTreeMap<&str, ContextBlockRecord>> {
    let mut selected = std::collections::BTreeMap::new();
    for record in records {
        record.validate_format()?;
        if selected
            .insert(record.slot_id.as_str(), record.record)
            .is_some()
        {
            return Err(invalid("duplicate stable slot accounting"));
        }
    }
    Ok(selected)
}

/// Requires an unambiguous structural association before any input is adopted.
pub(super) fn validate_stable_sources(
    blocks: &[ContextBlock],
    sources: &[(usize, StableContextSlotId)],
) -> AgentContextResult<std::collections::BTreeMap<usize, StableContextSlotId>> {
    let mut selected = std::collections::BTreeMap::new();
    let mut keys = std::collections::BTreeSet::new();
    for (index, key) in sources {
        if blocks
            .get(*index)
            .is_none_or(|block| block.placement != super::ContextPlacement::StablePrefix)
            || !keys.insert(key.as_str())
            || selected.insert(*index, key.clone()).is_some()
        {
            return Err(invalid("invalid stable producer source association"));
        }
    }
    Ok(selected)
}

/// Requires an unambiguous structural association before any input is adopted.
fn validate_record_indices(
    blocks: usize,
    records: &[(usize, ContextBlockRecord)],
) -> AgentContextResult<()> {
    if records.iter().any(|(index, _)| *index >= blocks)
        || records.windows(2).any(|pair| pair[0].0 >= pair[1].0)
    {
        return Err(invalid("invalid record projection indices"));
    }
    Ok(())
}

/// Hashes the exact renderer revision without concatenating/copying its body.
fn rendered_digest(header: &str, content: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mez-context-block-rendering-v1\0");
    hash.update((header.len() as u64).to_le_bytes());
    hash.update(header.as_bytes());
    hash.update((content.len() as u64).to_le_bytes());
    hash.update(content.as_bytes());
    hash.finalize().into()
}

/// Reports content-free import failures, never source text or authored metadata.
fn invalid(reason: &str) -> AgentContextError {
    AgentContextError::new(format!("context block record: {reason}"))
}

#[cfg(test)]
thread_local! {
    /// A test-thread-only counter, absent from production and unrelated owners.
    pub(super) static ESTIMATE_COMPUTATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
