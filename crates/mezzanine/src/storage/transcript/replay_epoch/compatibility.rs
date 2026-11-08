//! Semantic admission for historical compaction formats.
//!
//! A selective sidecar owns several distinct model-authored summaries. It cannot
//! become one typed head by concatenation or by reopening covered raw source.
//! Refuse that source before accounting import, receipt recovery or adoption.
//! Historical decoding remains available to integrity/edit/audit owners; this
//! bounded header probe grants no validity, normalization or publication proof.

use super::{AgentTranscriptStore, MANIFEST_LIMIT, MANIFEST_NAME, Result, bounded_read, invalid};
use serde::Deserialize;

impl AgentTranscriptStore {
    /// Checks semantic replay admission under the caller's existing conversation
    /// mutation lease. Never acquires another flock, decodes audit history,
    /// repairs a cache, acknowledges a receipt or writes source state. Compatible
    /// formats still require their complete normal validation before adoption.
    pub(in crate::storage::transcript) fn check_replay_compatibility_locked(
        &self,
        conversation: &str,
    ) -> Result<()> {
        let path = self.session_dir_path(conversation)?.join(MANIFEST_NAME);
        let bytes = match bounded_read(&path, MANIFEST_LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        #[derive(Deserialize)]
        struct Header {
            manifest: Option<serde::de::IgnoredAny>,
            version: Option<u64>,
            #[serde(default)]
            ranges: Vec<serde::de::IgnoredAny>,
        }
        let header: Header =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid epoch authority"))?;
        if header.manifest.is_none()
            && !header.ranges.is_empty()
            && let Some(version @ (2 | 3)) = header.version
        {
            return Err(invalid(&format!(
                "legacy selective compaction version {version} requires explicit single-head normalization",
            )));
        }
        Ok(())
    }
}
