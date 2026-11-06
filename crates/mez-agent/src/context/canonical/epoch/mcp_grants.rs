//! Retrieval availability across accepted rewrites and restrictive source copies.
//!
//! Revocation preserves exact model evidence and original accounting. It grants
//! no execution authority and is independent of trusted current configuration.
//! The set is bounded by retained chronological retrieval occurrences. Checkpoint
//! restoration preserves explicit denials and derives the minimum old-epoch
//! denials when importing the original checkpoint format.

use super::*;

impl AgentContext {
    /// Queries descriptive retrieval availability without interpreting a label
    /// or removing evidence. Actual native action admission remains separate.
    pub(crate) fn mcp_retrieval_is_current(&self, id: ContextBlockOccurrenceId) -> bool {
        !self.retired_mcp_retrievals.contains(&id)
    }

    pub(in super::super) fn prune_retired_mcp_retrievals(&mut self) {
        if self.retired_mcp_retrievals.is_empty() {
            return;
        }
        let retained = self
            .chronology
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::McpRetrievedManifest)
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        self.retired_mcp_retrievals
            .retain(|id| retained.contains(id));
    }
}

impl ReplayEpochContext {
    /// Revokes callable tools represented by current retrieval evidence while
    /// preserving its bytes, records, ordering and costs. Fresh retrieval with
    /// its own occurrence can grant again. Source copies/forks may use this
    /// restrictive transition without changing revision; it mints no permission,
    /// effect or persistence receipt. Accepted rewrites call it once, while
    /// duplicate adoption leaves later retrievals unchanged.
    pub fn revoke_retrieved_mcp_grants(&mut self) {
        self.context.retired_mcp_retrievals.extend(
            self.context
                .chronology
                .iter()
                .filter(|event| event.block().source == ContextSourceKind::McpRetrievedManifest)
                .map(|event| event.record().occurrence_id()),
        );
    }

    /// Minimum denials implied by the actual accepted epoch, without inventing
    /// grant rights for older retained source or altering its birth revision.
    pub(super) fn older_mcp_retrievals(&self) -> BTreeSet<ContextBlockOccurrenceId> {
        self.context
            .chronology()
            .iter()
            .skip(usize::from(self.head.is_some()))
            .zip(&self.members)
            .filter(|(event, member)| {
                event.block().source == ContextSourceKind::McpRetrievedManifest
                    && member.born_in < self.revision
            })
            .map(|(event, _)| event.record().occurrence_id())
            .collect()
    }

    /// Denials may be stricter than birth metadata (for example a source copy),
    /// but cannot name missing/non-retrieval source or omit an older grant.
    pub(super) fn validate_mcp_retrievals(&self) -> AgentContextResult<()> {
        let retained = self
            .context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::McpRetrievedManifest)
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        if !self.context.retired_mcp_retrievals.is_subset(&retained)
            || !self
                .older_mcp_retrievals()
                .is_subset(&self.context.retired_mcp_retrievals)
        {
            return Err(AgentContextError::new(
                "replay MCP retrieval revocation drifted",
            ));
        }
        Ok(())
    }
}
