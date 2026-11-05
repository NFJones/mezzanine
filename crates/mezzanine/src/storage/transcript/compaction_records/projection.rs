//! Borrowed, bounded semantic epoch serialization without accounting payloads.
//!
//! Legacy qualification hashes the original exact field shape. The old encoded
//! semantic budget also remains in force after v3 adds record space. Streaming
//! into a bounded sink avoids cloning summary bodies or allocating another
//! complete encoded epoch merely to measure or qualify its metadata.

use super::{AgentCompactionEpoch, invalid};
use crate::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

/// Original field order and empty-range omission shared with v1/v2 encoding.
#[derive(Serialize)]
struct SemanticProjection<'a> {
    version: u64,
    conversation_id: &'a str,
    through_sequence: u64,
    summary: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ranges: &'a Vec<super::super::AgentCompactionRange>,
}

/// Bounded byte measurement, optionally hashing the same serialized stream.
struct ProjectionSink {
    bytes: usize,
    hasher: Option<Sha256>,
}

impl Write for ProjectionSink {
    /// Refuses before the original encoded semantic budget is exceeded. V3's
    /// metadata allowance also contains its final file terminator.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("compaction semantic size exhausted"))?;
        if next as u64 > super::super::store::COMPACTION_EPOCH_MAX_BYTES {
            return Err(io::Error::other(
                "compaction semantic projection exceeds encoded size limit",
            ));
        }
        self.bytes = next;
        if let Some(hasher) = &mut self.hasher {
            hasher.update(bytes);
        }
        Ok(bytes.len())
    }

    /// No buffered filesystem or external state belongs to this sink.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serializes only borrowed semantic fields into the bounded sink.
fn project(epoch: &AgentCompactionEpoch, version: u64, hash: bool) -> Result<ProjectionSink> {
    let projection = SemanticProjection {
        version,
        conversation_id: &epoch.conversation_id,
        through_sequence: epoch.through_sequence,
        summary: &epoch.summary,
        ranges: &epoch.ranges,
    };
    let mut sink = ProjectionSink {
        bytes: 0,
        hasher: hash.then(Sha256::new),
    };
    serde_json::to_writer(&mut sink, &projection).map_err(|error| {
        invalid(format!(
            "compaction semantic projection encode failed: {error}"
        ))
    })?;
    Ok(sink)
}

/// Validates legacy-equivalent content size independently of record overhead.
pub(super) fn validate_summary_payload_size(epoch: &AgentCompactionEpoch) -> Result<()> {
    project(epoch, if epoch.ranges.is_empty() { 1 } else { 2 }, false).map(|_| ())
}

/// Qualifies the exact old semantic shape without copying complete bodies.
pub(super) fn legacy_projection_digest(
    epoch: &AgentCompactionEpoch,
    version: u64,
) -> Result<String> {
    let hasher = project(epoch, version, true)?
        .hasher
        .ok_or_else(|| invalid("legacy compaction digest is unavailable"))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
