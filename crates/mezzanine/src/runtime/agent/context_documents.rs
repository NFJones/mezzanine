//! Future-turn injection for explicitly enabled persisted context documents.

use super::{ContextBlock, ContextSourceKind, Result, RuntimeSessionService};
use mez_agent::{
    AgentContext, StableContextBlock, StableContextSlotId, StableContextSourceFingerprint,
};

impl RuntimeSessionService {
    /// Adds a deterministic snapshot of enabled documents to one newly built turn context.
    pub(in crate::runtime) fn apply_persisted_context_documents(
        &self,
        pane_id: &str,
        mut context: AgentContext,
    ) -> Result<AgentContext> {
        let Some(config_root) = self.integration.config_root() else {
            return Ok(context);
        };
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .unwrap_or_else(|| config_root.to_path_buf());
        let project = crate::security::project::discover_project_root(&working_directory)
            .to_string_lossy()
            .into_owned();
        let store =
            crate::storage::context_documents::ContextDocumentStore::under_config_root(config_root);
        let selection = store.select_enabled_for_project(&project)?;
        let slots = selection
            .documents
            .into_iter()
            .map(|document| {
                let revision = store.revision(&document)?;
                StableContextBlock::new(
                    StableContextSlotId::new(format!("context-document-{}", document.id))?,
                    StableContextSourceFingerprint::new(revision)?,
                    ContextBlock::stable_instruction(
                        ContextSourceKind::PersistedContextDocument,
                        format!(
                            "persisted context document {} ({})",
                            document.title, document.id
                        ),
                        document.content,
                    ),
                )
                .map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?;
        context
            .replace_stable_source_slots(ContextSourceKind::PersistedContextDocument, slots)
            .map_err(crate::error::MezError::from)?;
        context.validate_durable()?;
        Ok(context)
    }
}
