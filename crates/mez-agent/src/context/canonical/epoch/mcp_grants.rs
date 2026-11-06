//! Retrieval availability across accepted rewrites and restrictive source copies.
//!
//! Revocation preserves exact model evidence and original accounting. It grants
//! no execution authority and is independent of trusted current configuration.
//! The set is bounded by retained chronological retrieval occurrences. Checkpoint
//! restoration preserves explicit denials and derives the minimum old-epoch
//! denials when importing the original checkpoint format.

use super::*;

/// Original retrieval epoch, carried in the immutable retrieval evidence so a
/// delayed ordinary append cannot become a fresh grant after a head replacement.
/// This is an availability restriction, not a native action permission or ACK.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayMcpRetrievalScope {
    conversation: String,
    revision: u64,
}

impl AgentContext {
    pub(crate) fn mcp_retrieval_scope(&self) -> Option<&ReplayMcpRetrievalScope> {
        self.mcp_retrieval_scope.as_ref()
    }

    /// Unscoped original records can only grant before the first accepted
    /// rewrite. Missing provenance after that point cannot establish freshness.
    pub(crate) fn legacy_mcp_retrieval_is_current(&self) -> bool {
        self.mcp_retrieval_scope
            .as_ref()
            .is_none_or(|scope| scope.revision == 0)
    }

    pub(crate) fn mcp_retrieval_scope_matches(&self, value: &serde_json::Value) -> bool {
        serde_json::from_value::<Option<ReplayMcpRetrievalScope>>(value.clone())
            .ok()
            .is_some_and(|scope| {
                scope.as_ref() == self.mcp_retrieval_scope.as_ref()
                    || scope.is_none() && self.legacy_mcp_retrieval_is_current()
            })
    }

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
    /// Binds availability to the actual checked owner. This metadata has no
    /// independent history body and is reconstructed from the selected epoch.
    pub(super) fn bind_mcp_retrieval_scope(&mut self) {
        self.context.mcp_retrieval_scope = Some(ReplayMcpRetrievalScope {
            conversation: self.conversation.as_str().to_owned(),
            revision: self.revision.get(),
        });
    }

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
        if self.context.mcp_retrieval_scope.as_ref()
            != Some(&ReplayMcpRetrievalScope {
                conversation: self.conversation.as_str().to_owned(),
                revision: self.revision.get(),
            })
        {
            return Err(AgentContextError::new("replay MCP retrieval scope drifted"));
        }
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
