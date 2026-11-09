//! Bounded proof of non-model content changes within one canonical epoch.
//!
//! Every revision points directly to the original unedited epoch receipt. Repeat
//! edits flatten against that same base, so ordinary replay never walks a chain
//! of edits or resolves covered model source. The enclosing mutation owner must
//! separately qualify the actual parent, raw rows, index and archive replacement;
//! this descriptive proof grants no write, append or execution permission.

use super::*;

mod content;
mod transaction;

/// Required immutable receipt of the unedited current head, independent of the
/// immediately preceding storage CAS parent. A copied root remains independently
/// backed; its provenance is retained in this receipt rather than reinterpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRevisionOrigin {
    pub(super) base: Payload,
}

impl RawRevisionOrigin {
    /// Header-only validation precedes any reference resolution. A raw revision
    /// must advance storage CAS while preserving the canonical epoch and head.
    pub(super) fn validate(&self, manifest: &Manifest) -> Result<()> {
        self.base.validate_format()?;
        if self.base.bytes as u64 > MANIFEST_LIMIT
            || !matches!(manifest.version, 11 | MODE_MANIFEST_VERSION)
            || manifest.is_source_preserving_root()
            || manifest.expected_parent.is_none()
            || manifest.parent_head != manifest.head
        {
            return Err(invalid("raw revision has invalid original provenance"));
        }
        Ok(())
    }

    /// Checks the one original head receipt and proves that only retained
    /// content and its recomputed estimates changed. Covered source payloads
    /// are not resolved: their original receipt references remain unchanged.
    pub(super) fn verify(
        &self,
        manifest: &Manifest,
        current: &ReplayEpochContext,
        resolve: &mut dyn FnMut(Payload) -> Result<String>,
    ) -> Result<()> {
        let bytes = resolve(self.base.clone())?;
        self.base.validate(bytes.as_bytes())?;
        let base: Envelope = serde_json::from_str(&bytes)
            .map_err(|_| invalid("invalid original raw revision receipt"))?;
        base.validate(&manifest.conversation_id)?;
        if base.manifest.raw_revision_origin.is_some()
            || base.manifest.version > MODE_MANIFEST_VERSION
            || manifest.version == 11 && base.manifest.version > 10
            || base.manifest.epoch_revision != manifest.epoch_revision
            || base.manifest.head != manifest.head
            || base.manifest.committed_head_policy() != current.head_policy()
            || base
                .manifest
                .archive_cut
                .as_ref()
                .map(|cut| cut.through_sequence())
                != manifest
                    .archive_cut
                    .as_ref()
                    .map(|cut| cut.through_sequence())
            || base.manifest.revision >= manifest.revision
            || manifest.expected_parent.as_ref().is_none_or(|parent| {
                parent.revision < base.manifest.revision
                    || (parent.revision == base.manifest.revision && parent != &base.identity())
            })
        {
            return Err(invalid("raw revision is not based on its original head"));
        }
        // The explicit non-nesting guard above bounds this restoration to one
        // base receipt. It never selects the base as an alternative history.
        let original = base.restore(AgentContext::empty(), &mut *resolve)?;
        let mut expected_checkpoint = inline_checkpoint(&base.manifest.checkpoint, &original)?;
        let offset = usize::from(original.head().is_some());
        for (old, new) in original
            .context()
            .chronology()
            .iter()
            .skip(offset)
            .zip(current.context().chronology().iter().skip(offset))
        {
            if old.block().content != new.block().content {
                expected_checkpoint = expected_checkpoint
                    .revise_raw_member(old.record().occurrence_id(), new.block().content.clone())
                    .map_err(context_error)?;
            }
        }
        if expected_checkpoint != inline_checkpoint(&manifest.checkpoint, current)? {
            return Err(invalid(
                "raw revision changed protected checkpoint metadata",
            ));
        }
        let mut expected = base.manifest;
        expected.version = manifest.version;
        expected.head_policy =
            (manifest.version == MODE_MANIFEST_VERSION).then_some(current.head_policy());
        expected.revision = manifest.revision;
        expected.publication = manifest.publication;
        expected.expected_parent = manifest.expected_parent.clone();
        expected.parent_head = expected.head;
        expected.copy_origin = None;
        expected.memory_copy_origin = None;
        expected.prefix_origin = None;
        expected.raw_revision_origin = Some(self.clone());
        expected.checkpoint = manifest.checkpoint.clone();
        expected.archive_cut = manifest.archive_cut.clone();
        expected.source_escrow = expected.source_escrow.map(|items| {
            items
                .into_iter()
                .map(|item| item.with_revised_payload(current))
                .collect()
        });
        if &expected != manifest {
            return Err(invalid("raw revision changed original source custody"));
        }
        Ok(())
    }
}

/// Recovers the exact DTO using already resolved bytes. Unlike checkpointing a
/// restored owner, this preserves original format, stable records and cached
/// metadata so canonical restoration cannot hide unrelated normalization.
pub(super) fn inline_checkpoint(
    checkpoint: &ReplayEpochCheckpoint<Payload>,
    owner: &ReplayEpochContext,
) -> Result<ReplayEpochCheckpoint> {
    let events = owner
        .context()
        .chronology()
        .iter()
        .map(|event| (event.record().occurrence_id(), &event.block().content))
        .collect::<BTreeMap<_, _>>();
    checkpoint.clone().try_map_payloads(|id, reference| {
        let content = events
            .get(&id)
            .ok_or_else(|| invalid("raw revision payload has no original occurrence"))?;
        reference.validate(content.as_bytes())?;
        Ok((*content).clone())
    })
}
