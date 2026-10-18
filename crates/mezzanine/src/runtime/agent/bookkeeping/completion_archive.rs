//! Original-target archive checking separate from immutable producer identity.
//!
//! The frozen candidate/read/hash are never rebound to C2 pane metadata. This
//! worker snapshot uses only accepted rows at the original store/path/CID and a
//! private in-process admission stamp. One coherent All view supplies committed
//! frontier and logical rows; a separate tail read cannot attest that view.
//! This is historical RAW archive proof, not current model/paint authority,
//! selective-compaction epoch publication, or a disk-durability acknowledgement.
//! Store append still checks terminal certificates and on-disk integrity.

use super::*;
use crate::runtime::persistence::{RuntimeTranscriptAdmissionStamp, RuntimeTranscriptNextSequence};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};

/// Accepted rows and admission identity sampled for one original-target read.
#[derive(Debug, Clone)]
pub(super) struct RuntimeCompletionArchiveRead {
    pub(super) stamp: RuntimeTranscriptAdmissionStamp,
    pub(super) pending: Vec<TranscriptEntry>,
    minimum_committed_through: u64,
    observed: Arc<OnceLock<RuntimeCompletionArchiveObservation>>,
}

/// Proof coupled to the actual worker result, not populated from actor counters.
#[derive(Debug, Clone)]
pub(super) struct RuntimeCompletionArchiveObservation {
    pub(super) committed_through: u64,
    pub(super) logical_through: u64,
    pub(super) next: RuntimeTranscriptNextSequence,
    logical_digest: String,
}

/// Explicit archive identity projection, independent of lower-crate serialization.
#[derive(serde::Serialize)]
struct ArchiveRowIdentity<'a> {
    conversation_id: &'a str,
    sequence: u64,
    created_at_unix_seconds: u64,
    role: &'static str,
    turn_id: &'a str,
    agent_id: &'a str,
    pane_id: &'a str,
    content: &'a str,
}

impl<'a> From<&'a TranscriptEntry> for ArchiveRowIdentity<'a> {
    /// Exhaustive destructuring makes additions to row identity require a decision.
    fn from(entry: &'a TranscriptEntry) -> Self {
        let TranscriptEntry {
            conversation_id,
            sequence,
            created_at_unix_seconds,
            role,
            turn_id,
            agent_id,
            pane_id,
            content,
        } = entry;
        Self {
            conversation_id,
            sequence: *sequence,
            created_at_unix_seconds: *created_at_unix_seconds,
            role: match role {
                TranscriptRole::User => "user",
                TranscriptRole::Assistant => "assistant",
                TranscriptRole::Tool => "tool",
                TranscriptRole::System => "system",
            },
            turn_id,
            agent_id,
            pane_id,
            content,
        }
    }
}

/// Hashes a fixed-field JSON array projection: escaping, row boundaries and
/// array order are unambiguous, including empty strings and embedded delimiters.
/// No raw row or encoded private content is included in diagnostics.
fn rows_digest(entries: &[TranscriptEntry]) -> Result<String> {
    let rows: Vec<_> = entries.iter().map(ArchiveRowIdentity::from).collect();
    let bytes = serde_json::to_vec(&rows)
        .map_err(|_| MezError::invalid_state("original archive row identity cannot be encoded"))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

impl RuntimeCompletionArchiveRead {
    /// Executes once under the store's coherent checked-view lock. Historical
    /// pending in producer identity is not current archive ownership: use only
    /// the separately captured exact-target queue/in-flight rows.
    pub(super) fn execute(
        &self,
        read: &RuntimeBookkeepingTranscriptReadWork,
    ) -> Result<(Vec<TranscriptEntry>, u64)> {
        if self.observed.get().is_some() {
            return Err(MezError::invalid_state(
                "original archive read work cannot execute twice",
            ));
        }
        if read.store.transcript_path(&read.conversation_id)? != self.stamp.path
            || read.conversation_id != self.stamp.conversation_id
        {
            return Err(MezError::invalid_state(
                "original archive read target changed",
            ));
        }
        let view = read.store.conversation_transcript_view(
            &read.conversation_id,
            ConversationTranscriptRead::All,
            read.committed_prefix_required || self.minimum_committed_through > 0,
            &self.pending,
        )?;
        let committed_through = view.committed.last().map_or(0, |entry| entry.sequence);
        if committed_through < self.minimum_committed_through {
            return Err(MezError::invalid_state(
                "required original transcript prefix is missing or shortened",
            ));
        }
        let next = committed_through
            .checked_add(1)
            .map(RuntimeTranscriptNextSequence::Next)
            .unwrap_or(RuntimeTranscriptNextSequence::Exhausted);
        self.observed
            .set(RuntimeCompletionArchiveObservation {
                committed_through,
                logical_through: view.logical.last().map_or(0, |entry| entry.sequence),
                next,
                logical_digest: rows_digest(&view.logical)?,
            })
            .map_err(|_| MezError::invalid_state("original archive observation already exists"))?;
        // This transport scalar is not used as MAX+1. The paired observation
        // explicitly marks exhaustion, and actor admission rejects a new delta.
        let transport_next = match next {
            RuntimeTranscriptNextSequence::Next(next) => next,
            RuntimeTranscriptNextSequence::Exhausted => u64::MAX,
            RuntimeTranscriptNextSequence::Unknown => {
                return Err(MezError::invalid_state(
                    "checked archive sequence is unknown",
                ));
            }
        };
        Ok((view.logical, transport_next))
    }

    /// Validates the private current owner and exact worker result before append
    /// admission. Normal drains may remove pending rows only after confirmed
    /// original-target success; cancellation/error/new acceptance rotates identity.
    pub(super) fn validate(
        &self,
        service: &RuntimeSessionService,
        read: &RuntimeBookkeepingTranscriptReadWork,
        history: &[TranscriptEntry],
        transport_next: u64,
    ) -> Result<RuntimeCompletionArchiveObservation> {
        if read.store.transcript_path(&read.conversation_id)? != self.stamp.path
            || read.conversation_id != self.stamp.conversation_id
            || !service
                .persistence
                .transcript_admission_matches(&self.stamp)
        {
            return Err(MezError::invalid_state(
                "accepted original archive history changed before transcript acceptance",
            ));
        }
        let observed = self.observed.get().ok_or_else(|| {
            MezError::invalid_state("original archive read has no checked observation")
        })?;
        let expected_next = match observed.next {
            RuntimeTranscriptNextSequence::Next(next) => next,
            RuntimeTranscriptNextSequence::Exhausted => u64::MAX,
            RuntimeTranscriptNextSequence::Unknown => {
                return Err(MezError::invalid_state(
                    "original archive sequence observation is missing",
                ));
            }
        };
        if transport_next != expected_next || rows_digest(history)? != observed.logical_digest {
            return Err(MezError::invalid_state(
                "original archive result differs from its coherent observation",
            ));
        }
        let reservation_owned = match self.stamp.next {
            RuntimeTranscriptNextSequence::Unknown => true,
            RuntimeTranscriptNextSequence::Next(next) => {
                next > 0 && next.saturating_sub(1) <= observed.logical_through
            }
            RuntimeTranscriptNextSequence::Exhausted => observed.logical_through == u64::MAX,
        };
        if !reservation_owned {
            return Err(MezError::invalid_state(
                "original archive reservation has no checked logical row owner",
            ));
        }
        let current = service
            .persistence
            .pending_transcript_entries_at_path(&self.stamp.path, &self.stamp.conversation_id)?;
        for row in &current {
            if !history.iter().any(|entry| entry == row) {
                return Err(MezError::invalid_state(
                    "original archive accepted rows changed during check",
                ));
            }
        }
        let current_stamp = service
            .persistence
            .transcript_admission_committed_through(&self.stamp)?;
        for row in &self.pending {
            if !current.iter().any(|entry| entry == row)
                && row.sequence > current_stamp.max(observed.committed_through)
            {
                return Err(MezError::invalid_state(
                    "original archive pending owner disappeared without checked success",
                ));
            }
        }
        if current_stamp > observed.logical_through {
            return Err(MezError::invalid_state(
                "original archive committed prefix advanced outside checked history",
            ));
        }
        Ok(observed.clone())
    }
}

impl RuntimeSessionService {
    /// Captures existing original-target owners, never current pane/session data.
    pub(super) fn capture_completion_archive_read(
        &mut self,
        candidate: &RuntimeBookkeepingCandidate,
    ) -> Result<RuntimeCompletionArchiveRead> {
        let read = &candidate.read;
        let path = read.store.transcript_path(&read.conversation_id)?;
        if read.conversation_id != candidate.turn.conversation_id {
            return Err(MezError::invalid_state(
                "original archive conversation differs from candidate",
            ));
        }
        let pending = self
            .persistence
            .pending_transcript_entries_at_path(&path, &read.conversation_id)?;
        let stamp = self
            .persistence
            .capture_transcript_admission_stamp(&path, &read.conversation_id);
        let original_pending = read
            .pending
            .iter()
            .map(|entry| entry.sequence)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;
        let minimum_committed_through = read
            .transcript_entries
            .saturating_sub(original_pending)
            .max(stamp.committed_through);
        Ok(RuntimeCompletionArchiveRead {
            stamp,
            pending,
            minimum_committed_through,
            observed: Arc::new(OnceLock::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds inert synthetic row data without involving archive I/O or effects.
    fn row() -> TranscriptEntry {
        TranscriptEntry {
            conversation_id: "conversation".into(),
            sequence: 1,
            created_at_unix_seconds: 2,
            role: TranscriptRole::User,
            turn_id: "turn".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content: "content".into(),
        }
    }

    /// Every canonical field must affect identity, even when content/sequence
    /// match. All four roles must have distinct stable projection labels, so a
    /// metadata-only substitution cannot attest the original coherent read.
    #[test]
    fn archive_digest_covers_every_field_and_role() -> Result<()> {
        let original = row();
        let digest = rows_digest(std::slice::from_ref(&original))?;
        let mut variants = Vec::new();
        let mut changed = original.clone();
        changed.conversation_id.push('x');
        variants.push(changed);
        let mut changed = original.clone();
        changed.sequence += 1;
        variants.push(changed);
        let mut changed = original.clone();
        changed.created_at_unix_seconds += 1;
        variants.push(changed);
        for role in [
            TranscriptRole::Assistant,
            TranscriptRole::Tool,
            TranscriptRole::System,
        ] {
            let mut changed = original.clone();
            changed.role = role;
            variants.push(changed);
        }
        let mut changed = original.clone();
        changed.turn_id.push('x');
        variants.push(changed);
        let mut changed = original.clone();
        changed.agent_id.push('x');
        variants.push(changed);
        let mut changed = original.clone();
        changed.pane_id.push('x');
        variants.push(changed);
        let mut changed = original.clone();
        changed.content.push('x');
        variants.push(changed);
        let mut digests = std::collections::BTreeSet::from([digest.clone()]);
        for changed in variants {
            let changed_digest = rows_digest(&[changed])?;
            assert_ne!(digest, changed_digest);
            assert!(digests.insert(changed_digest));
        }
        assert_eq!(digest, rows_digest(&[original])?);
        Ok(())
    }

    /// JSON framing must distinguish field-boundary shifts, escaped content,
    /// row count, duplicate rows and order. Concatenating raw strings or hashing
    /// a set of rows would incorrectly equate at least one of these cases.
    #[test]
    fn archive_digest_preserves_field_row_and_order_boundaries() -> Result<()> {
        let mut first = row();
        first.turn_id = "ab".into();
        first.agent_id = "c".into();
        first.content = "\"},\\\n\0[{".into();
        let mut shifted = first.clone();
        shifted.turn_id = "a".into();
        shifted.agent_id = "bc".into();
        assert_ne!(
            rows_digest(&[first.clone()])?,
            rows_digest(&[shifted.clone()])?
        );
        assert_ne!(rows_digest(&[])?, rows_digest(&[first.clone()])?);
        assert_ne!(
            rows_digest(&[first.clone()])?,
            rows_digest(&[first.clone(), first.clone()])?
        );
        assert_ne!(
            rows_digest(&[first.clone(), shifted.clone()])?,
            rows_digest(&[shifted.clone(), first.clone()])?
        );
        let mut joined = first.clone();
        joined.content.push_str(&shifted.content);
        assert_ne!(rows_digest(&[joined])?, rows_digest(&[first, shifted])?);
        Ok(())
    }
}
