//! Versioned persistence of semantic context and its descriptive accounting.
//!
//! This wrapper leaves the existing semantic event codec intact. Ordinary
//! consumers unwrap only that event; owned replay also receives the occurrence
//! and validated cache input. Saved records cannot install instructions, infer
//! execution ownership or become provider messages. Legacy semantic records
//! remain an explicit unaccounted import boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ContextBlockRecord, StableContextBlockRecord};

use super::{TranscriptContextEvent, TranscriptContractError};

/// Reserved persisted-accounting marker; it is never semantic model content.
pub const ACCOUNTED_CONTEXT_RECORD_MARKER: &str = "[mez-transcript-context-record/v1]\n";
const VERSION: &str = "mez-transcript-context-record/v1";
const FAMILY: &str = "[mez-transcript-context-record/";
const MAX_STABLE_RECORDS: usize = 1024;
const MAX_STABLE_SLOT_BYTES: usize = 4096;
// The largest semantic field already has the provider-response ceiling. Allow
// worst-case JSON escaping and bounded metadata, then refuse before parsing.
const MAX_RECORD_BYTES: usize = crate::http::DEFAULT_PROVIDER_MAX_RESPONSE_BYTES * 8;

/// Semantic source plus non-model-visible occurrence/cost snapshots.
#[derive(Clone, PartialEq, Eq)]
pub struct TranscriptContextRecord {
    event: TranscriptContextEvent,
    record: ContextBlockRecord,
    stable: Option<Vec<StableContextBlockRecord>>,
}

/// Strict wire envelope independent of the legacy semantic event's own version.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    version: String,
    event: Value,
    record: ContextBlockRecord,
    stable: Option<Vec<StableContextBlockRecord>>,
}

/// Bounded framing probe that skips semantic payload allocation when no stable
/// claim exists. It is not a semantic replay decoder.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StableProbe {
    version: String,
    #[serde(rename = "event")]
    _event: serde::de::IgnoredAny,
    record: ContextBlockRecord,
    stable: Option<serde::de::IgnoredAny>,
}

impl TranscriptContextRecord {
    /// Captures an actual producer's semantic event and accounting snapshot.
    /// Only current bounded metadata formats are accepted. Semantic constructors
    /// and the canonical owner retain their source/causal validation duties.
    pub fn new(
        event: TranscriptContextEvent,
        record: ContextBlockRecord,
        stable: Option<Vec<StableContextBlockRecord>>,
    ) -> Result<Self, TranscriptContractError> {
        record.validate_format().map_err(|_| invalid())?;
        let slots_input = stable.as_deref().unwrap_or_default();
        if slots_input.len() > MAX_STABLE_RECORDS {
            return Err(invalid());
        }
        let mut slots = std::collections::BTreeSet::new();
        for slot in slots_input {
            if slot.slot_id().len() > MAX_STABLE_SLOT_BYTES || !slots.insert(slot.slot_id()) {
                return Err(invalid());
            }
            slot.validate_format().map_err(|_| invalid())?;
        }
        Ok(Self {
            event,
            record,
            stable,
        })
    }

    /// Borrows only semantic content for display/provider/compactor projection.
    pub fn event(&self) -> &TranscriptContextEvent {
        &self.event
    }

    /// Returns descriptive accounting for later checked canonical restoration.
    pub fn record(&self) -> ContextBlockRecord {
        self.record
    }

    /// Returns an explicit stable snapshot, including intentional empty state.
    /// None means this occurrence supplies no stable-slot snapshot. The active
    /// owner always supplies instructions.
    pub fn stable_records(&self) -> Option<&[StableContextBlockRecord]> {
        self.stable.as_deref()
    }

    /// Moves semantic source and metadata separately for one-pass replay parsing.
    pub fn into_parts(
        self,
    ) -> (
        TranscriptContextEvent,
        ContextBlockRecord,
        Option<Vec<StableContextBlockRecord>>,
    ) {
        (self.event, self.record, self.stable)
    }

    /// Discards accounting when a caller only consumes the semantic event.
    pub fn into_event(self) -> TranscriptContextEvent {
        self.event
    }

    /// Encodes the immutable wrapper without changing the semantic source.
    /// Encoding/size failures return no publishable record.
    pub fn to_transcript_content(&self) -> Result<String, TranscriptContractError> {
        let wire = WireRecord {
            version: VERSION.to_owned(),
            event: self.event.to_record_value(),
            record: self.record,
            stable: self.stable.clone(),
        };
        let payload = serde_json::to_string(&wire).map_err(|_| invalid())?;
        if payload
            .len()
            .saturating_add(ACCOUNTED_CONTEXT_RECORD_MARKER.len())
            > MAX_RECORD_BYTES
        {
            return Err(invalid());
        }
        Ok(format!("{ACCOUNTED_CONTEXT_RECORD_MARKER}{payload}"))
    }

    /// Distinguishes legacy/unrelated text from malformed or future accounting.
    /// A recognized family with unsupported framing returns an explicit error;
    /// callers must not quietly replay a partial source after such a failure.
    /// Cached costs remain descriptive until checked against actual rendering.
    pub fn from_transcript_content(content: &str) -> Result<Option<Self>, TranscriptContractError> {
        if !is_accounted_context_record(content) {
            return Ok(None);
        }
        if content.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let payload = content
            .strip_prefix(ACCOUNTED_CONTEXT_RECORD_MARKER)
            .ok_or_else(invalid)?;
        let wire: WireRecord = serde_json::from_str(payload).map_err(|_| invalid())?;
        if wire.version != VERSION {
            return Err(invalid());
        }
        #[cfg(test)]
        SEMANTIC_DECODES.with(|count| count.set(count.get() + 1));
        let event = TranscriptContextEvent::from_record_value(&wire.event).ok_or_else(invalid)?;
        Self::new(event, wire.record, wire.stable).map(Some)
    }

    /// Reads only stable carriers without reconstructing covered semantic
    /// payloads that explicitly have no snapshot. Carrier events still pass the
    /// full decoder, including empty-slot tombstones. Unsupported framing and
    /// record formats fail. A `None` is no stable claim, not semantic validity:
    /// every selected model source must still use the full decoder above.
    pub fn stable_snapshot_from_transcript_content(
        content: &str,
    ) -> Result<Option<Self>, TranscriptContractError> {
        if !Self::has_stable_snapshot(content)? {
            return Ok(None);
        }
        Self::from_transcript_content(content)
    }

    /// Probes supported accounting framing without reconstructing semantic
    /// payloads or snapshots. True identifies a candidate requiring full decode;
    /// false proves no stable claim, never that the model source is valid.
    pub fn has_stable_snapshot(content: &str) -> Result<bool, TranscriptContractError> {
        if !is_accounted_context_record(content) {
            return Ok(false);
        }
        if content.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let payload = content
            .strip_prefix(ACCOUNTED_CONTEXT_RECORD_MARKER)
            .ok_or_else(invalid)?;
        let probe: StableProbe = serde_json::from_str(payload).map_err(|_| invalid())?;
        if probe.version != VERSION {
            return Err(invalid());
        }
        probe.record.validate_format().map_err(|_| invalid())?;
        Ok(probe.stable.is_some())
    }
}

#[cfg(test)]
thread_local! {
    /// Counts actual semantic deserialization, independently of lookup callers.
    static SEMANTIC_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl std::fmt::Debug for TranscriptContextRecord {
    /// Logs only occurrence/count facts, never authored or private source text.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TranscriptContextRecord")
            .field("occurrence", &self.record.occurrence_id())
            .field("stable_slots", &self.stable.as_ref().map_or(0, Vec::len))
            .finish_non_exhaustive()
    }
}

/// Identifies this reserved family, including unsupported future versions.
pub fn is_accounted_context_record(content: &str) -> bool {
    content.starts_with(FAMILY)
}

/// Keeps failures content-free; raw source and metadata are not diagnostics.
fn invalid() -> TranscriptContractError {
    TranscriptContractError::new("invalid or unsupported accounted context record")
}

#[cfg(test)]
mod tests;
