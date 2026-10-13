//! Immutable transcript-only presentation evidence carried by the existing log.
//!
//! A fork keeps its new transcript/catalog creation time, but a bounded original
//! transcript tail remains presentation source. Records use the existing TSV
//! codec, preserving original identity, coarse time, roles and exact bytes. Live
//! fallback publication may instead retain already formatted typed rows. Neither
//! form is one semantic occurrence: runtime projects each proven record boundary
//! separately and leaves chrome explicitly untagged. Geometry, filtering, styles
//! and installation remain runtime responsibilities; this module performs no I/O.

use crate::error::{MezError, Result};
use mez_agent::transcript::TranscriptEntry;
use mez_core::presentation::PresentationEntry;
use serde::{Deserialize, Serialize};

/// Version-one source envelope, independent of transcript/config schema versions.
pub(crate) const CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.transcript-fallback-v1+json; charset=utf-8";
/// Same bounded record tail used by resume/restart, before JSON/TSV escaping.
pub(crate) const RECORD_LIMIT: usize = 64;
/// Maximum original encoded tail retained for transcript-only forks.
pub(crate) const RECORD_BYTES: usize = 2 * 1024 * 1024;
/// Allows reversible JSON escaping of the bounded TSV tail, not unbounded input.
const SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Immutable source with explicit versioning and no inferred occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    /// Envelope schema, not display or execution authority.
    version: u8,
    /// Original record evidence or an already accepted typed projection.
    pub content: Content,
}

/// Source variants distinguish untagged chrome from missing legacy evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Content {
    /// Original captured tail, rendered with the current outer conversation chrome.
    Records {
        /// Full captured transcript count, independent of the retained tail.
        transcript_entries: usize,
        /// Original canonical TSV records; never loaded as model context here.
        records: Vec<String>,
    },
    /// Existing fallback visible source and explicit per-record metadata.
    Rows {
        /// None is intentional chrome, not an unknown semantic occurrence.
        lines: Vec<(String, Option<PresentationEntry>)>,
    },
}

impl Source {
    /// Captures the original tail without changing its time, identity or bytes.
    pub(crate) fn from_records(entries: &[TranscriptEntry]) -> Result<Self> {
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for entry in entries.iter().rev().take(RECORD_LIMIT) {
            let record = super::encoding::encode_transcript_entry(entry)?;
            bytes = bytes.saturating_add(record.len() + 1);
            if bytes > RECORD_BYTES {
                break;
            }
            records.push(record);
        }
        records.reverse();
        Ok(Self {
            version: 1,
            content: Content::Records {
                transcript_entries: entries.len(),
                records,
            },
        })
    }

    /// Retains a formatted fallback's proven boundaries and intentional chrome.
    pub(crate) fn from_rows(lines: Vec<(String, Option<PresentationEntry>)>) -> Self {
        Self {
            version: 1,
            content: Content::Rows { lines },
        }
    }

    /// Encodes validated bounded source; no mutable state or clock is consulted.
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate()?;
        let encoded = serde_json::to_string(self).map_err(|error| {
            MezError::invalid_args(format!("encode transcript fallback: {error}"))
        })?;
        if encoded.len() > SOURCE_BYTES {
            return Err(MezError::invalid_args(
                "transcript fallback source exceeds limit",
            ));
        }
        Ok(encoded)
    }

    /// Decodes source strictly; corrupt canonical payload never becomes raw UI text.
    pub(crate) fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > SOURCE_BYTES {
            return Err(MezError::invalid_args(
                "transcript fallback source exceeds limit",
            ));
        }
        let source: Self = serde_json::from_str(encoded).map_err(|error| {
            MezError::invalid_args(format!("decode transcript fallback: {error}"))
        })?;
        source.validate()?;
        Ok(source)
    }

    /// Decodes original records through the canonical transcript contract.
    pub(crate) fn entries(records: &[String]) -> Result<Vec<TranscriptEntry>> {
        records
            .iter()
            .map(|record| super::encoding::decode_transcript_entry(record))
            .collect()
    }

    /// Rejects malformed versions, chronology, metadata and unbounded tails.
    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(MezError::invalid_args(
                "unsupported transcript fallback version",
            ));
        }
        match &self.content {
            Content::Records {
                transcript_entries,
                records,
            } => {
                if records.len() > RECORD_LIMIT
                    || records.len() > *transcript_entries
                    || records
                        .iter()
                        .map(|record| record.len().saturating_add(1))
                        .sum::<usize>()
                        > RECORD_BYTES
                {
                    return Err(MezError::invalid_args("invalid transcript fallback tail"));
                }
                let entries = Self::entries(records)?;
                if entries.windows(2).any(|pair| {
                    pair[0].conversation_id != pair[1].conversation_id
                        || pair[0].sequence >= pair[1].sequence
                }) {
                    return Err(MezError::invalid_args(
                        "transcript fallback chronology changed",
                    ));
                }
            }
            Content::Rows { lines } => {
                if lines.len() > RECORD_LIMIT + 3
                    || lines
                        .iter()
                        .any(|(_, entry)| entry.as_ref().is_some_and(|entry| !entry.is_valid()))
                {
                    return Err(MezError::invalid_args("invalid transcript fallback rows"));
                }
            }
        }
        Ok(())
    }
}
