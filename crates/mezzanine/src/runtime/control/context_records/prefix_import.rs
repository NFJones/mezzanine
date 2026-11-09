//! Historical prefix source and audit dependencies use the same real renderer.
//! Import preserves original reservations without recreating task completion or
//! execution rights. Only the storage owner can acknowledge atomic migration.

use super::*;

impl RuntimeContextRecords {
    /// Assigns typed ownership to this exact memory-only selection and retains
    /// its immutable original placement/producers for later copies. It performs
    /// no storage I/O or parent publication, including for pending arrivals.
    pub(in crate::runtime::control) fn freeze_legacy_prefix(
        &mut self,
        epoch: &crate::storage::transcript::AgentCompactionEpoch,
        blocks: Vec<ContextBlock>,
        execution: &[mez_agent::ImportedExecutionEvent],
        snapshot: &crate::storage::transcript::ConversationReplaySnapshot,
    ) -> Result<Vec<ContextBlock>> {
        let head = epoch
            .summary_records
            .as_ref()
            .and_then(|records| records.prefix)
            .ok_or_else(invalid)?;
        let owner = self.import_legacy_prefix(&epoch.conversation_id, blocks, execution, head)?;
        let mut row_records = self.row_cache.clone();
        row_records.extend(self.rendered_row_records()?);
        let memory =
            crate::storage::transcript::replay_epoch::ReplayMemorySource::capture_legacy_prefix(
                &owner,
                epoch,
                &snapshot.view.logical,
                &self.row_origins,
                &row_records,
                || {
                    // The same read supplies covered task backing. These logical
                    // rows are only pure audit input, never durable publication.
                    let mut view = snapshot.view.clone();
                    view.committed.clone_from(&view.logical);
                    let audit = crate::storage::transcript::ConversationReplaySnapshot {
                        epoch: None,
                        replay: None,
                        view,
                        row_origins: self.row_origins.clone(),
                        row_records: row_records.clone(),
                        stable_records: None,
                        initial_stable_records: None,
                        settled_pending_origins: Default::default(),
                    };
                    Self::import_legacy_prefix_backing(&audit)
                },
            )?;
        let (mut imported, blocks) =
            Self::from_replay_owner(owner.conversation_id().clone(), owner, None)?;
        let mut replay = imported.replay.take().ok_or_else(invalid)?;
        replay.memory = Some(std::sync::Arc::new(memory));
        // Preserve the actual renderer's row accounting and producer metadata;
        // replacing the whole transport would lose pending occurrence custody.
        self.records = imported.records;
        self.replay = Some(replay);
        Ok(blocks)
    }

    /// Converts a checked saved prefix, retaining explicit execution metadata
    /// and original sequence reservations before assigning typed ownership.
    pub(in crate::runtime::control) fn import_legacy_prefix(
        &self,
        conversation: &str,
        mut blocks: Vec<ContextBlock>,
        execution: &[mez_agent::ImportedExecutionEvent],
        head: ContextBlockRecord,
    ) -> Result<mez_agent::ReplayEpochContext> {
        if !self.task_admissions.is_empty() {
            self.ensure_valid()?;
            if blocks.is_empty() || self.records.first() != Some(&(0, head)) {
                return Err(invalid());
            }
            let first = blocks.remove(0);
            let mut suffix = self.clone();
            suffix.records = suffix
                .records
                .into_iter()
                .filter_map(|(index, record)| index.checked_sub(1).map(|index| (index, record)))
                .collect();
            let ids = suffix
                .records
                .iter()
                .map(|(_, record)| record.occurrence_id())
                .collect::<std::collections::BTreeSet<_>>();
            // A partly covered task retains its exact root as protected raw
            // input. Its original prelude remains source-only escrow backing;
            // it cannot be reintroduced as a new live task suffix.
            suffix
                .task_admissions
                .retain(|_, first| ids.contains(first));
            let conversation = mez_agent::ReplayConversationId::new(conversation)?;
            let source = replay_arrivals::bootstrap(conversation, blocks, &suffix, execution)?;
            return Ok(source.import_legacy_prefix_head(first, head, execution)?);
        }
        let mut context = self.import_composed_context(blocks, &[], execution, false, None)?;
        self.restore_prefix_reservations(&mut context)?;
        context.normalize_legacy_import_framing(execution)?;
        Ok(mez_agent::ReplayEpochContext::from_legacy_prefix(
            mez_agent::ReplayConversationId::new(conversation)?,
            context,
            head,
        )?)
    }

    /// Projects original covered task-start backing under the migration lease.
    /// This callback does no I/O and acquires no lease. It restores descriptive
    /// task history, never a committed head or executable rights.
    /// Its changed descriptive costs return to the existing leased cache owner;
    /// covered occurrences never enter the migrated model membership.
    pub(in crate::runtime::control) fn import_legacy_prefix_backing(
        snapshot: &crate::storage::transcript::ConversationReplaySnapshot,
    ) -> Result<(AgentContext, Vec<(u64, ContextBlockRecord)>)> {
        let first = snapshot.view.committed.first().ok_or_else(invalid)?;
        let mut history =
            super::super::context::runtime_agent_transcript_audit_context_from_snapshot(
                &first.pane_id,
                snapshot,
            );
        if history.provider_history_repair_identity.is_some() {
            return Err(MezError::conflict(
                "legacy task backing requires native history repair",
            ));
        }
        history.records.qualify_execution_controls(
            None,
            &first.conversation_id,
            &snapshot.view.committed,
        )?;
        let updates = history.records.committed_row_records(snapshot)?;
        if !history.records.task_admissions.is_empty() {
            let conversation = mez_agent::ReplayConversationId::new(&first.conversation_id)?;
            let source = replay_arrivals::bootstrap(
                conversation,
                history.blocks,
                &history.records,
                &history.execution_events,
            )?;
            return Ok((source.into_owned_context(), updates));
        }
        let mut context = history.records.import_composed_context(
            history.blocks,
            &[],
            &history.execution_events,
            false,
            None,
        )?;
        history.records.restore_prefix_reservations(&mut context)?;
        Ok((context, updates))
    }

    fn restore_prefix_reservations(&self, context: &mut AgentContext) -> Result<()> {
        let reservations = context
            .chronology()
            .iter()
            .filter_map(|event| {
                self.source_sequences
                    .get(&event.record().occurrence_id())
                    .map(|sequence| (event.record().occurrence_id(), *sequence))
            })
            .collect::<Vec<_>>();
        context.restore_imported_history_sequences(&reservations)?;
        Ok(())
    }
}
