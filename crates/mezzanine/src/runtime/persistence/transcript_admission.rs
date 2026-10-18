//! Original-target transcript admission metadata in the existing reservation map.
//!
//! This is not a payload queue, history map, filesystem epoch or permission
//! capability. A private allocation identity qualifies in-process work against
//! the same path/CID admission owner; strong work handles prevent allocation ABA.
//! Queued/in-flight rows remain owned by their existing effect queues. Only
//! exact successful write/checked-read observations establish committed prefix.
//! Metadata is bounded per target/live work epoch, not globally capped: the
//! preexisting reservation map already has no global target budget.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::RuntimePersistenceComponent;
use super::RuntimeSideEffect;
use crate::error::{MezError, Result};
use mez_agent::transcript::TranscriptEntry;

/// MAX has no representable successor; it must not saturate into a reusable slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeTranscriptNextSequence {
    Unknown,
    Next(u64),
    Exhausted,
}

/// Private allocation handle. Arc value equality would accept every empty token.
#[derive(Clone)]
struct AdmissionIdentity(Arc<()>);

impl PartialEq for AdmissionIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for AdmissionIdentity {}
impl std::fmt::Debug for AdmissionIdentity {
    /// Identity is opaque and confers no external/native authority.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AdmissionIdentity")
    }
}

/// Exclusively actor-owned live slot: intentionally not Clone or serializable.
#[derive(Debug)]
pub(super) struct TranscriptAdmission {
    next: RuntimeTranscriptNextSequence,
    reservation_active: bool,
    legacy_next: Option<u64>,
    legacy_active: bool,
    committed_through: u64,
    identity: AdmissionIdentity,
}

impl TranscriptAdmission {
    /// Creates a new epoch, never imported from old work/projection state.
    fn new() -> Self {
        Self {
            next: RuntimeTranscriptNextSequence::Unknown,
            reservation_active: false,
            legacy_next: None,
            legacy_active: false,
            committed_through: 0,
            identity: AdmissionIdentity(Arc::new(())),
        }
    }

    /// Canceled cursors cannot allocate gaps; confirmed MAX stays exhausted.
    fn checked_next(&self) -> RuntimeTranscriptNextSequence {
        if self.committed_through == u64::MAX {
            RuntimeTranscriptNextSequence::Exhausted
        } else if self.reservation_active {
            self.next
        } else {
            RuntimeTranscriptNextSequence::Unknown
        }
    }
}

/// Non-authorizing snapshot of the existing original-target admission owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeTranscriptAdmissionStamp {
    pub(crate) path: PathBuf,
    pub(crate) conversation_id: String,
    pub(crate) next: RuntimeTranscriptNextSequence,
    pub(crate) committed_through: u64,
    identity: AdmissionIdentity,
}

impl RuntimePersistenceComponent {
    /// Snapshots rows from existing owners at the exact original target. Foreign
    /// CIDs, mixed store/path producers and same-sequence conflicting bytes fail
    /// closed; only full-field identical rows collapse across queue/in-flight.
    pub(crate) fn pending_transcript_entries_at_path(
        &self,
        path: &Path,
        conversation_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        fn insert(
            rows: &mut BTreeMap<u64, TranscriptEntry>,
            batch: &[TranscriptEntry],
            conversation_id: &str,
        ) -> Result<()> {
            for entry in batch {
                entry.validate().map_err(|_| {
                    MezError::invalid_state("original transcript pending entry is invalid")
                })?;
                if entry.conversation_id != conversation_id {
                    return Err(MezError::invalid_state(
                        "original transcript target contains a foreign conversation",
                    ));
                }
                if let Some(previous) = rows.get(&entry.sequence) {
                    if previous != entry {
                        return Err(MezError::invalid_state(
                            "original transcript pending sequence has conflicting entries",
                        ));
                    }
                } else {
                    rows.insert(entry.sequence, entry.clone());
                }
            }
            Ok(())
        }
        let mut rows = BTreeMap::new();
        for ((owner, _), batches) in &self.in_flight_transcript_entries {
            for (owned_path, entries) in batches {
                if owned_path != path {
                    continue;
                }
                if owner != conversation_id {
                    return Err(MezError::invalid_state(
                        "original transcript in-flight owner changed",
                    ));
                }
                insert(&mut rows, entries, conversation_id)?;
            }
        }
        for effect in &self.queued_transcript_effects {
            if let RuntimeSideEffect::PersistTranscriptEntries {
                path: owned_path,
                store,
                entries,
            } = effect
            {
                if owned_path != path {
                    continue;
                }
                if store.transcript_path(conversation_id)? != path {
                    return Err(MezError::invalid_state(
                        "original transcript queued store differs from its target",
                    ));
                }
                insert(&mut rows, entries, conversation_id)?;
            }
        }
        Ok(rows.into_values().collect())
    }

    /// Validates new completion allocation before any reservation/queue mutation.
    /// False means identical rows already have an actual pending owner, not a
    /// manufactured transport acknowledgement or disk-durability statement.
    pub(crate) fn queue_checked_completion_transcript(
        &mut self,
        effect: RuntimeSideEffect,
    ) -> Result<bool> {
        let RuntimeSideEffect::PersistTranscriptEntries {
            path,
            store,
            entries,
        } = &effect
        else {
            return Err(MezError::invalid_state(
                "completion append is not a transcript effect",
            ));
        };
        let first = entries
            .first()
            .ok_or_else(|| MezError::invalid_state("completion append is empty"))?;
        if store.transcript_path(&first.conversation_id)? != *path {
            return Err(MezError::invalid_state(
                "completion append store differs from original target",
            ));
        }
        for entry in entries {
            entry
                .validate()
                .map_err(|_| MezError::invalid_state("completion append entry is invalid"))?;
            if entry.conversation_id != first.conversation_id {
                return Err(MezError::invalid_state(
                    "completion append contains a foreign conversation",
                ));
            }
        }
        if entries
            .windows(2)
            .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(MezError::invalid_state(
                "completion append sequence is not contiguous or exhausted",
            ));
        }
        let pending = self.pending_transcript_entries_at_path(path, &first.conversation_id)?;
        let mut already_owned = 0usize;
        for entry in entries {
            if let Some(existing) = pending
                .iter()
                .find(|existing| existing.sequence == entry.sequence)
            {
                if existing != entry {
                    return Err(MezError::invalid_state(
                        "completion append conflicts with accepted original rows",
                    ));
                }
                already_owned += 1;
            }
        }
        if already_owned == entries.len() {
            return Ok(false);
        }
        if already_owned != 0 {
            return Err(MezError::invalid_state(
                "completion append overlaps an accepted original prefix",
            ));
        }
        if self
            .deferred_transcript_next_sequences
            .get(&(path.clone(), first.conversation_id.clone()))
            .is_some_and(|slot| slot.checked_next() == RuntimeTranscriptNextSequence::Exhausted)
        {
            return Err(MezError::invalid_state(
                "completion transcript sequence is exhausted",
            ));
        }
        self.queue_transcript(effect);
        Ok(true)
    }

    /// Captures a real slot even when no rows exist; None is not an ABA proof.
    pub(crate) fn capture_transcript_admission_stamp(
        &mut self,
        path: &Path,
        conversation_id: &str,
    ) -> RuntimeTranscriptAdmissionStamp {
        let key = (path.to_path_buf(), conversation_id.to_string());
        let slot = self
            .deferred_transcript_next_sequences
            .entry(key)
            .or_insert_with(TranscriptAdmission::new);
        RuntimeTranscriptAdmissionStamp {
            path: path.to_path_buf(),
            conversation_id: conversation_id.to_string(),
            next: slot.checked_next(),
            committed_through: slot.committed_through,
            identity: slot.identity.clone(),
        }
    }

    /// Checks identity and logical admission only. Successful drains may advance
    /// committed-through without invalidating a snapshot containing those rows.
    pub(crate) fn transcript_admission_matches(
        &self,
        stamp: &RuntimeTranscriptAdmissionStamp,
    ) -> bool {
        self.deferred_transcript_next_sequences
            .get(&(stamp.path.clone(), stamp.conversation_id.clone()))
            .is_some_and(|slot| {
                slot.identity == stamp.identity && slot.checked_next() == stamp.next
            })
    }

    /// Records a queue acceptance at its actual target. Legacy queue admission
    /// remains void/non-rejecting; strict completion allocation validates first.
    pub(crate) fn reserve_transcript_sequence_at_path(
        &mut self,
        path: &Path,
        conversation_id: &str,
        last_sequence: u64,
    ) {
        let slot = self
            .deferred_transcript_next_sequences
            .entry((path.to_path_buf(), conversation_id.to_string()))
            .or_insert_with(TranscriptAdmission::new);
        slot.identity = AdmissionIdentity(Arc::new(()));
        if !slot.reservation_active {
            slot.next = RuntimeTranscriptNextSequence::Unknown;
        }
        if !slot.legacy_active {
            slot.legacy_next = None;
        }
        slot.legacy_next = Some(
            slot.legacy_next
                .unwrap_or(0)
                .max(last_sequence.saturating_add(1)),
        );
        slot.legacy_active = true;
        slot.reservation_active = true;
        let next = last_sequence
            .checked_add(1)
            .map(RuntimeTranscriptNextSequence::Next)
            .unwrap_or(RuntimeTranscriptNextSequence::Exhausted);
        slot.next = match (slot.next, next) {
            (RuntimeTranscriptNextSequence::Exhausted, _)
            | (_, RuntimeTranscriptNextSequence::Exhausted) => {
                RuntimeTranscriptNextSequence::Exhausted
            }
            (
                RuntimeTranscriptNextSequence::Next(old),
                RuntimeTranscriptNextSequence::Next(new),
            ) => RuntimeTranscriptNextSequence::Next(old.max(new)),
            (_, next) => next,
        };
    }

    /// Invalidates work before cancel/error diagnostics can fail. In-flight rows
    /// are not erased and prior confirmed prefix is not rewritten as empty.
    pub(crate) fn invalidate_transcript_admission_at_path(
        &mut self,
        path: &Path,
        conversation_id: &str,
    ) {
        if let Some(slot) = self
            .deferred_transcript_next_sequences
            .get_mut(&(path.to_path_buf(), conversation_id.to_string()))
        {
            slot.identity = AdmissionIdentity(Arc::new(()));
        }
    }

    /// Records only an actual owned append success at the original target.
    /// The caller validates its exact path/sequence/rows before invoking this.
    pub(crate) fn observe_committed_transcript_at_path(
        &mut self,
        path: &Path,
        conversation_id: &str,
        through: u64,
    ) {
        let slot = self
            .deferred_transcript_next_sequences
            .entry((path.to_path_buf(), conversation_id.to_string()))
            .or_insert_with(TranscriptAdmission::new);
        slot.committed_through = slot.committed_through.max(through);
        if through == u64::MAX {
            slot.next = RuntimeTranscriptNextSequence::Exhausted;
        }
    }

    /// Reads only the confirmed prefix of the same current private owner.
    pub(crate) fn transcript_admission_committed_through(
        &self,
        stamp: &RuntimeTranscriptAdmissionStamp,
    ) -> Result<u64> {
        if !self.transcript_admission_matches(stamp) {
            return Err(MezError::invalid_state(
                "original transcript admission owner changed",
            ));
        }
        self.deferred_transcript_next_sequences
            .get(&(stamp.path.clone(), stamp.conversation_id.clone()))
            .map(|slot| slot.committed_through)
            .ok_or_else(|| {
                MezError::invalid_state("original transcript admission owner is missing")
            })
    }

    /// Records an actual owned coherent historical raw read, not pending rows,
    /// current pane counters, epoch publication or a supplied replacement token.
    pub(crate) fn observe_checked_transcript_read(
        &mut self,
        stamp: &RuntimeTranscriptAdmissionStamp,
        through: u64,
    ) -> Result<()> {
        if !self.transcript_admission_matches(stamp) {
            return Err(MezError::invalid_state(
                "original transcript admission changed before read observation",
            ));
        }
        self.observe_committed_transcript_at_path(&stamp.path, &stamp.conversation_id, through);
        Ok(())
    }

    /// Legacy CID lookup preserves its previous max-across-target semantics.
    /// It is compatibility bookkeeping, never original-path acceptance proof.
    pub(super) fn legacy_deferred_transcript_next_sequence(
        &self,
        conversation_id: &str,
    ) -> Option<u64> {
        self.deferred_transcript_next_sequences
            .iter()
            .filter(|((_, owner), slot)| owner == conversation_id && slot.legacy_active)
            .filter_map(|(_, slot)| slot.legacy_next)
            .max()
    }

    /// Cancellation invalidates every extant target, including in-flight-only
    /// slots. Retain confirmed-prefix truth; remove only unconfirmed quiescent
    /// reservations as the legacy API did. Old work keeps its old token alive.
    pub(crate) fn cancel_transcript_admissions_for_conversation(&mut self, conversation_id: &str) {
        let mut active_paths = BTreeMap::<PathBuf, u64>::new();
        for ((owner, _), batches) in &self.in_flight_transcript_entries {
            if owner != conversation_id {
                continue;
            }
            for (path, entries) in batches {
                if let Some(last) = entries.last() {
                    let frontier = active_paths.entry(path.clone()).or_insert(0);
                    *frontier = (*frontier).max(last.sequence);
                }
            }
        }
        let legacy_in_flight = !active_paths.is_empty();
        self.deferred_transcript_next_sequences
            .retain(|(path, owner), slot| {
                if owner != conversation_id {
                    return true;
                }
                slot.identity = AdmissionIdentity(Arc::new(()));
                let in_flight = active_paths.get(path).copied();
                slot.next = in_flight
                    .map(|through| through.max(slot.committed_through))
                    .and_then(|through| through.checked_add(1))
                    .map(RuntimeTranscriptNextSequence::Next)
                    .unwrap_or(
                        if in_flight.is_some() || slot.committed_through == u64::MAX {
                            RuntimeTranscriptNextSequence::Exhausted
                        } else {
                            RuntimeTranscriptNextSequence::Unknown
                        },
                    );
                slot.reservation_active = in_flight.is_some();
                slot.legacy_active = legacy_in_flight;
                if !legacy_in_flight {
                    slot.legacy_next = None;
                }
                legacy_in_flight || slot.committed_through > 0
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::transcript::AgentTranscriptStore;

    /// Produces valid immutable bytes without opening files or spawning work.
    fn row(sequence: u64, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            conversation_id: "archive-owner".to_string(),
            sequence,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-owner".to_string(),
            agent_id: "agent-owner".to_string(),
            pane_id: "%1".to_string(),
            content: text.to_string(),
        }
    }

    /// Builds only an effect value; these metadata tests never service its I/O.
    fn effect(store: &AgentTranscriptStore, rows: Vec<TranscriptEntry>) -> RuntimeSideEffect {
        RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path("archive-owner").unwrap(),
            store: store.clone(),
            entries: rows,
        }
    }

    /// Queue-to-inflight preserves authority, and only exact successful owned
    /// settlement advances committed truth. Same-CID path B stays independent;
    /// a different byte payload cannot consume A's write or its stamp.
    #[test]
    fn transcript_admission_exact_drain_preserves_stamp_and_path_isolation() {
        let mut owner = RuntimePersistenceComponent::default();
        let a = AgentTranscriptStore::new(PathBuf::from("archive-a"));
        let b = AgentTranscriptStore::new(PathBuf::from("archive-b"));
        let path_a = a.transcript_path("archive-owner").unwrap();
        let path_b = b.transcript_path("archive-owner").unwrap();
        let other = owner.capture_transcript_admission_stamp(&path_b, "archive-owner");
        let entries = vec![row(1, "accepted")];
        assert!(
            owner
                .queue_checked_completion_transcript(effect(&a, entries.clone()))
                .unwrap()
        );
        let stamp = owner.capture_transcript_admission_stamp(&path_a, "archive-owner");
        assert_eq!(stamp.next, RuntimeTranscriptNextSequence::Next(2));
        assert_eq!(stamp.committed_through, 0);
        assert_eq!(owner.take_transcript_effects().len(), 1);
        assert!(owner.transcript_admission_matches(&stamp));
        assert!(!owner.settle_transcript_write_at_path(
            "archive-owner",
            1,
            &path_a,
            &[row(1, "foreign")]
        ));
        assert_eq!(
            owner
                .transcript_admission_committed_through(&stamp)
                .unwrap(),
            0
        );
        assert!(owner.settle_transcript_write_at_path("archive-owner", 1, &path_a, &entries));
        assert!(owner.transcript_admission_matches(&stamp));
        assert_eq!(
            owner
                .transcript_admission_committed_through(&stamp)
                .unwrap(),
            1
        );
        assert!(
            owner
                .pending_transcript_entries_at_path(&path_a, "archive-owner")
                .unwrap()
                .is_empty()
        );
        assert!(owner.transcript_admission_matches(&other));
    }

    /// Failure/cancellation rotate the strong owner before diagnostics; queued
    /// cancellation cannot erase inflight rows. Removing and recreating an empty
    /// slot never revives either an old stamp or its clone, even at identical
    /// path/CID/next/frontier values. The old strong handles stay alive here.
    #[test]
    fn transcript_admission_failure_cancel_and_recreate_reject_old_handles() {
        let mut owner = RuntimePersistenceComponent::default();
        let store = AgentTranscriptStore::new(PathBuf::from("archive-cancel"));
        let path = store.transcript_path("archive-owner").unwrap();
        let empty = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        let empty_clone = empty.clone();
        owner.cancel_queued_transcript_entries_for_conversation("archive-owner");
        let recreated = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        assert_eq!(empty.next, recreated.next);
        assert!(!owner.transcript_admission_matches(&empty));
        assert!(!owner.transcript_admission_matches(&empty_clone));
        let entries = vec![row(1, "inflight retained")];
        owner
            .queue_checked_completion_transcript(effect(&store, entries.clone()))
            .unwrap();
        owner.take_transcript_effects();
        let inflight = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        owner.invalidate_transcript_admission_at_path(&path, "archive-owner");
        assert!(!owner.transcript_admission_matches(&inflight));
        assert_eq!(
            owner
                .pending_transcript_entries_at_path(&path, "archive-owner")
                .unwrap(),
            entries
        );
        let failed = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        owner.cancel_queued_transcript_entries_for_conversation("archive-owner");
        assert!(!owner.transcript_admission_matches(&failed));
        assert_eq!(
            owner
                .pending_transcript_entries_at_path(&path, "archive-owner")
                .unwrap(),
            entries
        );
        let cancelled = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        assert_eq!(cancelled.committed_through, 0);
    }

    /// Identical pending MAX bytes dedup without a successor; conflicting bytes
    /// reject before mutation. Confirmed MAX remains explicitly exhausted even
    /// after cancellation, while the legacy CID cursor still saturates to MAX.
    #[test]
    fn transcript_admission_checked_max_dedup_conflict_and_exhaustion_are_distinct() {
        let mut owner = RuntimePersistenceComponent::default();
        let store = AgentTranscriptStore::new(PathBuf::from("archive-max"));
        let path = store.transcript_path("archive-owner").unwrap();
        let entries = vec![row(u64::MAX, "last row")];
        assert!(
            owner
                .queue_checked_completion_transcript(effect(&store, entries.clone()))
                .unwrap()
        );
        assert!(
            !owner
                .queue_checked_completion_transcript(effect(&store, entries.clone()))
                .unwrap()
        );
        assert!(
            owner
                .queue_checked_completion_transcript(effect(
                    &store,
                    vec![row(u64::MAX, "conflict")]
                ))
                .is_err()
        );
        let stamp = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        assert_eq!(stamp.next, RuntimeTranscriptNextSequence::Exhausted);
        assert_eq!(
            owner.legacy_deferred_transcript_next_sequence("archive-owner"),
            Some(u64::MAX)
        );
        assert_eq!(owner.take_transcript_effects().len(), 1);
        assert!(owner.settle_transcript_write_at_path("archive-owner", u64::MAX, &path, &entries));
        owner.cancel_queued_transcript_entries_for_conversation("archive-owner");
        let committed = owner.capture_transcript_admission_stamp(&path, "archive-owner");
        assert_eq!(committed.committed_through, u64::MAX);
        assert_eq!(committed.next, RuntimeTranscriptNextSequence::Exhausted);
        assert!(
            owner
                .queue_checked_completion_transcript(effect(
                    &store,
                    vec![row(u64::MAX, "new after MAX")]
                ))
                .is_err()
        );
        assert!(owner.take_transcript_effects().is_empty());
    }
}
