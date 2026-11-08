//! One canonical execution occurrence map for ordinary archive projection and
//! message-prefix capture. Group ordinals span the complete original chronology;
//! this encoder neither reconstructs summary witnesses nor changes replay authority.

use super::*;

impl RuntimeSessionService {
    /// Encodes typed execution occurrences over the complete canonical
    /// chronology, including imported predecessors. Prefix/turn boundaries
    /// never reset a group's ordinal; legacy unowned blocks remain unowned.
    /// Invalid payloads and exhausted ordinals fail before archive admission.
    pub(in crate::runtime) fn canonical_execution_occurrences(
        chronology: &[mez_agent::ConversationEvent],
    ) -> Result<std::collections::BTreeMap<u64, mez_agent::TranscriptContextEvent>> {
        let mut ordinals = std::collections::BTreeMap::<_, u64>::new();
        let mut occurrences = std::collections::BTreeMap::new();
        for event in chronology {
            let block = event.block();
            if !matches!(
                block.source,
                ContextSourceKind::CommittedEvidence
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
                    | ContextSourceKind::ActionResult
                    | ContextSourceKind::McpServerReference
                    | ContextSourceKind::McpServerSearchResult
                    | ContextSourceKind::McpRetrievedManifest
            ) {
                continue;
            }
            let Some(group) = event.execution_group_id() else {
                continue;
            };
            let ordinal = ordinals.entry(group.clone()).or_default();
            *ordinal = ordinal.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("canonical execution occurrence ordinal exhausted")
            })?;
            let source = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                block.source,
                block.label.clone(),
                block.content.clone(),
                group.clone(),
                *ordinal,
                event.provider_owner().cloned(),
            )
            .ok_or_else(|| {
                MezError::invalid_state("canonical execution occurrence cannot be encoded")
            })?;
            occurrences.insert(event.sequence().get(), source);
        }
        Ok(occurrences)
    }
}
