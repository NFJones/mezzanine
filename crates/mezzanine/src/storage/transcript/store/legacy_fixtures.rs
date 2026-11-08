//! Historical sidecar fixtures for integrity, editing and format-admission tests.
//!
//! These test-only installers create checked old persistence data directly.
//! They are separate from the product writer, which must refuse selective
//! publication. No replay owner, model result or controller witness is minted.

use super::*;

impl AgentTranscriptStore {
    /// Installs valid historical range data, importing its original summary
    /// records through the existing v2-to-v3 accounting contract.
    pub(crate) fn install_legacy_compaction_ranges_fixture(
        &self,
        conversation: &str,
        through_sequence: u64,
        summary: &str,
        ranges: Vec<AgentCompactionRange>,
    ) -> Result<()> {
        self.install_legacy_compaction_fixture(AgentCompactionEpoch {
            version: 2,
            conversation_id: conversation.into(),
            through_sequence,
            summary: summary.into(),
            ranges,
            summary_records: None,
        })
    }

    /// Accepts only a valid historical projection over actual synthetic archive
    /// rows. Existing v3 occurrence records survive unchanged. A typed authority
    /// cannot be overwritten even by this fixture-only boundary.
    pub(crate) fn install_legacy_compaction_fixture(
        &self,
        mut epoch: AgentCompactionEpoch,
    ) -> Result<()> {
        validate_conversation_id(&epoch.conversation_id)?;
        let _lease = self.acquire_conversation_lock(&epoch.conversation_id)?;
        if self.has_replay_manifest(&epoch.conversation_id)? {
            return Err(MezError::conflict(
                "legacy fixture cannot replace typed authority",
            ));
        }
        let rows = self.inspect_validated_transcript(&epoch.conversation_id)?;
        let next = rows.last().map_or(1, |row| row.sequence.saturating_add(1));
        if !matches!(epoch.version, 1..=3)
            || epoch.through_sequence >= next
            || (epoch.summary.trim().is_empty() && epoch.ranges.is_empty())
            || (epoch.version == 1 && !epoch.ranges.is_empty())
        {
            return Err(MezError::invalid_args("invalid historical epoch fixture"));
        }
        validate_compaction_ranges(&epoch.ranges, epoch.through_sequence, next)?;
        validate_compaction_range_sources(&epoch.ranges, &rows)?;
        if matches!(epoch.version, 1 | 2) {
            epoch.import_legacy_summary_records()?;
        } else {
            epoch.restore_summary_records()?;
        }
        epoch.validate_summary_payload_size()?;
        let encoded = serde_json::to_vec(&epoch).map_err(|error| {
            MezError::invalid_state(format!("historical fixture encoding failed: {error}"))
        })?;
        if encoded.len().saturating_add(1) as u64 > ACCOUNTED_COMPACTION_EPOCH_MAX_BYTES {
            return Err(MezError::invalid_args(
                "historical epoch fixture exceeds size limit",
            ));
        }
        let path = self
            .session_dir_path(&epoch.conversation_id)?
            .join(COMPACTION_EPOCH_FILE_NAME);
        std_fs::write(&path, encoded)?;
        set_private_file_permissions(&path)?;
        self.require_compaction_epoch_locked(&epoch.conversation_id)?;
        self.upsert_catalog_from_files(&epoch.conversation_id, None)?;
        Ok(())
    }
}
