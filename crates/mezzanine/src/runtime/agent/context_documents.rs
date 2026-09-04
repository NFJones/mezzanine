//! Future-turn injection for explicitly enabled persisted context documents.

use super::{ContextBlock, ContextSourceKind, Result, RuntimeSessionService};
use mez_agent::AgentContext;

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
        let snapshots = selection
            .documents
            .into_iter()
            .map(|document| {
                Ok(ContextBlock::task_prelude(
                    ContextSourceKind::PersistedContextDocument,
                    format!(
                        "persisted context document {} ({})",
                        document.title, document.id
                    ),
                    document.content,
                ))
            })
            .collect::<Result<Vec<ContextBlock>>>()?;
        context
            .insert_task_preludes_before_active_user(snapshots)
            .map_err(crate::error::MezError::from)?;
        context.validate_durable()?;
        Ok(context)
    }
}
