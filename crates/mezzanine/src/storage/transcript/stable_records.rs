//! Stable snapshots captured during the existing checked archive read.
//!
//! Stable producers outlive a raw replay window or compaction coverage. Their
//! original records remain in actual typed archive carriers. This observer keeps
//! only the latest visible claim while the caller owns the mutation lease; it
//! adds no scan, cache, storage format, source authority or instruction bytes.
//! Unknown reserved records retain a content-free rejection until superseded by
//! a later complete snapshot. Visible semantic corruption still fails rendering.

use crate::{MezError, Result};
use mez_agent::{
    StableContextBlockRecord, TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord,
};
use std::sync::Arc;

/// One immutable original carrier, including an explicit empty-slot tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptStableSnapshot {
    sequence: u64,
    /// Original authored carrier identity, checked even when its row is hidden.
    pub(super) occurrence: Option<mez_agent::ContextBlockOccurrenceId>,
    records: Option<Arc<[StableContextBlockRecord]>>,
}

impl TranscriptStableSnapshot {
    /// Establishes a validated receipt-owned baseline before child row one.
    /// It has no physical child carrier and conveys no semantic authority.
    pub(super) fn from_initial(records: &[StableContextBlockRecord]) -> Self {
        Self {
            sequence: 0,
            occurrence: None,
            records: Some(Arc::from(records)),
        }
    }

    /// Physical carrier position qualifies chronological precedence only.
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Refuses unknown reserved provenance instead of borrowing an older claim.
    pub(crate) fn records(&self) -> Result<&[StableContextBlockRecord]> {
        self.records.as_deref().ok_or_else(|| {
            MezError::invalid_state(format!(
                "stable snapshot carrier at row {} is invalid",
                self.sequence
            ))
        })
    }
}

/// Single-pass observer bounded to the original caller's visible frontier.
#[derive(Default)]
pub(super) struct StableRecordCapture {
    limit: Option<u64>,
    scanned_through: u64,
    latest: Option<StableRecordCandidate>,
}

/// Unadopted latest candidate. Raw bytes remain private and only this final
/// carrier is semantically decoded, after the whole checked scan succeeds.
struct StableRecordCandidate {
    sequence: u64,
    content: Option<Arc<str>>,
}

impl StableRecordCapture {
    /// Freezes a historical/ephemeral bound before any source is inspected.
    pub(super) fn new(limit: Option<u64>) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// A through-read cannot adopt later carriers even without an explicit cap.
    pub(super) fn restrict(&mut self, through: u64) {
        self.limit = Some(self.limit.map_or(through, |limit| limit.min(through)));
    }

    /// Observes validated ascending source rows, skipping a second selected-view
    /// visit. Pending owners enter only after their full raw merge succeeds.
    pub(super) fn observe(&mut self, row: &TranscriptEntry) {
        if row.sequence <= self.scanned_through {
            return;
        }
        self.scanned_through = row.sequence;
        if self.limit.is_some_and(|limit| row.sequence > limit)
            || row.role != TranscriptRole::System
        {
            return;
        }
        let content = match TranscriptContextRecord::has_stable_snapshot(&row.content) {
            Ok(true) => Some(Arc::from(row.content.as_str())),
            Ok(false) => return,
            Err(_) => None,
        };
        self.latest = Some(StableRecordCandidate {
            sequence: row.sequence,
            content,
        });
    }

    /// Fully validates only the final candidate. Earlier carrier semantics
    /// cannot influence adoption after a complete later snapshot supersedes it.
    pub(super) fn finish(self) -> Option<TranscriptStableSnapshot> {
        self.latest.map(|candidate| {
            let decoded = candidate.content.as_deref().and_then(|content| {
                TranscriptContextRecord::stable_snapshot_from_transcript_content(content)
                    .ok()
                    .flatten()
            });
            let (records, occurrence) = match decoded {
                Some(record) => {
                    let (_, record, slots) = record.into_parts();
                    match slots {
                        Some(slots) => (Some(Arc::from(slots)), Some(record.occurrence_id())),
                        None => (None, None),
                    }
                }
                None => (None, None),
            };
            TranscriptStableSnapshot {
                sequence: candidate.sequence,
                occurrence,
                records,
            }
        })
    }
}
