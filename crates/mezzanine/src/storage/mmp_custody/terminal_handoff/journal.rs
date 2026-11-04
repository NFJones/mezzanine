//! Small bodyless first-terminal and physical-position evidence for u1/h1.
//!
//! Full source fingerprint covers the original native sender, message ID and
//! recipient intervals. Those variable strings stay in accepted/canonical rows,
//! not every lifecycle journal. Actual anchors or reserved source decoders must
//! qualify this descriptive evidence before it can authorize a storage result.

use super::*;
use mez_agent::messaging::MessageDeliveryTerminalSnapshot;
use mez_agent::messaging::{DeliveryTerminalReason, MessageAcceptanceDecodeLimits};
use serde::{Deserialize, Serialize};

/// Fixed-size source identity; never serialized application input or task rights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    store_id: [u8; 16],
    source_sha256: [u8; 32],
    sequence: u64,
    index: u32,
    reason: DeliveryTerminalReason,
    observed_at_ms: u64,
}

/// Descriptive s1 link to an actual original canonical journal, never proof.
pub(in crate::storage::mmp_custody) struct HandoffConsumption {
    pub sequence: u64,
    pub source_sha256: [u8; 32],
    pub binding_index: u32,
    pub terminal: MessageDeliveryTerminalSnapshot,
    pub canonical: MessageTerminalHandoffRecord,
}
impl Source {
    /// Freezes the exact actual original terminal, excluding variable payloads.
    pub(super) fn capture(
        anchor: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
    ) -> Result<Self> {
        Ok(Self {
            store_id,
            source_sha256: anchor.source_sha256(),
            sequence: anchor.sequence(),
            index: u32::try_from(anchor.binding_index()).map_err(|_| invalid())?,
            reason: anchor.terminal().reason,
            observed_at_ms: anchor.terminal().observed_at_ms,
        })
    }
    /// Uses the same distinct handoff domain as the actual lower anchor.
    pub(super) fn occurrence(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mez-terminal-canonical-handoff-v1\0");
        hash.update(self.store_id);
        hash.update(self.source_sha256);
        hash.update(self.index.to_le_bytes());
        hash.update(self.observed_at_ms.to_le_bytes());
        hash.update([match self.reason {
            DeliveryTerminalReason::Expired => 1,
            DeliveryTerminalReason::RecipientRetired => 2,
        }]);
        hash.finalize().into()
    }
}

/// Canonical preparation/result. h1 does not itself release the source outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::storage::mmp_custody) struct HandoffJournal {
    version: u32,
    pub(super) generation: u64,
    pub(super) source: Source,
    pub(super) transcript: CustodyTranscriptPlan,
}
impl HandoffJournal {
    /// Constructs a first physical plan only from an actual terminal anchor.
    pub(super) fn capture(
        anchor: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
        generation: u64,
        transcript: CustodyTranscriptPlan,
    ) -> Result<Self> {
        Ok(Self {
            version: 1,
            generation,
            source: Source::capture(anchor, store_id)?,
            transcript,
        })
    }
    /// Compares actual original source/before-image and original sender target.
    pub(super) fn matches(
        &self,
        anchor: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
    ) -> bool {
        Source::capture(anchor, store_id).is_ok_and(|source| source == self.source)
            && self.transcript.conversation_id() == anchor.native_owner().conversation_id()
    }
    /// Returns bounded original bytes, never copying message IDs or payloads.
    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        Ok(bytes)
    }
    /// Describes a qualified result for the lower port to seal after retirement.
    pub(super) fn record(&self, archive_observed: bool) -> MessageTerminalHandoffRecord {
        MessageTerminalHandoffRecord {
            store_id: self.source.store_id,
            occurrence: self.source.occurrence(),
            archive_sequence: self.transcript.archive_sequence,
            canonical_sha256: self.transcript.canonical_sha256,
            archive_observed,
        }
    }
}

impl MmpCustodyStore {
    /// Requalifies s1's complete original h1 relation in the same source/ACK
    /// snapshot. A decoded reference never replaces the writer's opaque proof.
    pub(in crate::storage::mmp_custody) fn qualify_handoff_consumption(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        value: &HandoffConsumption,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        let source = Source {
            store_id: value.canonical.store_id,
            source_sha256: value.source_sha256,
            sequence: value.sequence,
            index: value.binding_index,
            reason: value.terminal.reason,
            observed_at_ms: value.terminal.observed_at_ms,
        };
        let occurrence = source.occurrence();
        if value.terminal.released
            || occurrence != value.canonical.occurrence
            || read_record(transaction, &key_for("u1", &occurrence))?.is_some()
        {
            return Err(invalid());
        }
        let actual = self
            .read_handoff_in_snapshot(transaction, &key_for("h1", &occurrence), limits)?
            .ok_or_else(invalid)?;
        let expected = actual.record(false);
        if actual.source != source
            || value.canonical.archive_sequence != expected.archive_sequence
            || value.canonical.canonical_sha256 != expected.canonical_sha256
            || value.canonical.store_id != expected.store_id
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// Reads checked bounded metadata; no returned DTO grants a handoff token.
    pub(super) fn read_handoff_metadata(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<HandoffJournal>> {
        let Some(bytes) = read_record(transaction, key)? else {
            return Ok(None);
        };
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let journal: HandoffJournal = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let state = self.checked_state(transaction)?;
        let source = &journal.source;
        journal.transcript.validate()?;
        if journal.version != 1
            || journal.generation == 0
            || journal.generation > state.generation as u64
            || source.store_id.as_slice() != self.store_id
            || source.source_sha256 == [0; 32]
            || source.sequence == 0
            || source.sequence > state.watermark
            || source.index >= 4096
            || (key != key_for("u1", &source.occurrence())
                && key != key_for("h1", &source.occurrence()))
        {
            return Err(invalid());
        }
        Ok(Some(journal))
    }

    /// Qualifies actual source/t1/ACK/native history in the same snapshot with
    /// one reserved decoder quantum. No decoded body survives this function.
    pub(in crate::storage::mmp_custody) fn read_handoff_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<Option<HandoffJournal>> {
        let Some(journal) = self.read_handoff_metadata(transaction, key)? else {
            return Ok(None);
        };
        self.qualify_handoff_source(transaction, &journal.source, limits)?;
        if key.starts_with("u1:") {
            self.ensure_handoff_unreleased(transaction, &journal.source)?;
        }
        let (source, digest, _) =
            self.read_accepted_source_in_snapshot(transaction, journal.source.sequence, limits)?;
        if digest != journal.source.source_sha256
            || source
                .native_sender()
                .is_none_or(|owner| owner.conversation_id() != journal.transcript.conversation_id())
        {
            return Err(invalid());
        }
        Ok(Some(journal))
    }

    /// Verifies the exact first strict expiry in original storage, including
    /// full source/ACK contradictions. Retirement waits for its own validated
    /// observation format; neither native absence nor epoch difference suffices.
    pub(super) fn qualify_handoff_source(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        source: &Source,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        if source.store_id.as_slice() != self.store_id
            || source.reason != DeliveryTerminalReason::Expired
            || self.validate_accepted_occurrence_in_snapshot(transaction, source.sequence)?
                != source.source_sha256
        {
            return Err(invalid());
        }
        let mut found = None;
        self.visit_expiry_metadata(transaction, |key, frame| {
            if frame.sequence == source.sequence && frame.binding_indices.contains(&source.index) {
                if found.is_some()
                    || frame.source_sha256 != source.source_sha256
                    || frame.observed_at_ms != source.observed_at_ms
                {
                    return Err(invalid());
                }
                found = Some(key.to_owned());
            }
            Ok(())
        })?;
        let key = found.ok_or_else(invalid)?;
        self.read_stored_expiry_in_snapshot(transaction, &key, limits)?;
        Ok(())
    }

    /// An unfinished canonical operation cannot begin or continue after an
    /// actual horizon release. A completed h1 is historical fact, not a release.
    pub(super) fn ensure_handoff_unreleased(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        source: &Source,
    ) -> Result<()> {
        self.ensure_slot_not_consumed_in_snapshot(transaction, source.sequence, source.index)?;
        self.visit_release_metadata(transaction, |_, release| {
            if release.sequence == source.sequence
                && release
                    .entries
                    .iter()
                    .any(|(index, _)| *index == source.index)
            {
                return Err(invalid());
            }
            Ok(())
        })
    }
}
