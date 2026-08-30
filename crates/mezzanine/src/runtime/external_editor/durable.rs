//! Durable issue and memory adapters for runtime-owned external editors.

use super::session::{ExternalEditTarget, ExternalEditorCompletion};
use crate::error::{MezError, Result};
use crate::runtime::commands::{runtime_issue_database_path, runtime_issues_enabled};
use crate::runtime::{RuntimeSessionService, current_unix_seconds};
use crate::storage::context_documents::{
    CompareAndSwapContextDocumentResult, ContextDocumentScope, ContextDocumentStore,
};
use crate::storage::issues::{CompareAndSwapIssueTextResult, IssueStore, IssueTextField};
use crate::storage::memory::{CompareAndSwapMemoryContentResult, PersistentMemoryStore};

/// Target-specific durable settlement used by lifecycle retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurableExternalEditSettlement {
    /// The completion belongs to another target family.
    Unhandled,
    /// No changed content was applied, but the completion was consumed.
    Retained,
    /// Changed content was committed through the target CAS boundary.
    Applied,
    /// The target changed or disappeared, so the draft must remain recoverable.
    Conflicted,
}

impl RuntimeSessionService {
    /// Opens one issue body or notes field in the configured external editor.
    pub(crate) fn start_issue_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        issue_id: &str,
        field: IssueTextField,
    ) -> Result<()> {
        if !runtime_issues_enabled(self) {
            return Err(MezError::invalid_state(
                "external issue editing requires issues to be enabled",
            ));
        }
        let (store, project) = self.external_editor_issue_store(pane_id)?;
        let record = store
            .get_issue(project.clone(), issue_id.to_string())?
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "issue not found")
            })?;
        let expected_revision = store.issue_revision(&record)?;
        let original_content = match field {
            IssueTextField::Body => record.body.unwrap_or_default(),
            IssueTextField::Notes => record.notes.unwrap_or_default(),
        };
        let target = match field {
            IssueTextField::Body => ExternalEditTarget::IssueBody {
                project,
                issue_id: record.id,
                expected_revision,
            },
            IssueTextField::Notes => ExternalEditTarget::IssueNotes {
                project,
                issue_id: record.id,
                expected_revision,
            },
        };
        self.start_external_editor_session(
            primary_client_id,
            pane_id,
            target,
            original_content.clone(),
            original_content,
            true,
        )?;
        self.sync_tracked_pty_sizes()?;
        Ok(())
    }

    /// Opens one persistent-memory content field in the configured editor.
    pub(crate) fn start_memory_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        memory_id: &str,
    ) -> Result<()> {
        let store = self.external_editor_memory_store()?;
        let record = store.inspect(memory_id)?;
        let expected_revision = store.memory_revision(&record)?;
        let original_content = record.content;
        self.start_external_editor_session(
            primary_client_id,
            pane_id,
            ExternalEditTarget::MemoryContent {
                memory_id: record.id,
                expected_revision,
            },
            original_content.clone(),
            original_content,
            true,
        )?;
        self.sync_tracked_pty_sizes()?;
        Ok(())
    }

    /// Opens one authorized persisted context document in the configured editor.
    pub(crate) fn start_context_document_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        document_id: &str,
    ) -> Result<()> {
        let config_root = self.external_editor_config_root()?;
        let project = self.context_document_project_for_pane(pane_id, &config_root);
        let store = ContextDocumentStore::under_config_root(&config_root);
        let document = store.inspect(document_id)?.ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "context document not found",
            )
        })?;
        if !document.visible_to_project(&project) {
            return Err(MezError::forbidden(
                "context document does not belong to the active project",
            ));
        }
        let expected_revision = store.revision(&document)?;
        let target_project = match &document.scope {
            ContextDocumentScope::Global => None,
            ContextDocumentScope::Project { root } => Some(root.clone()),
        };
        let original_content = document.content;
        self.start_external_editor_session(
            primary_client_id,
            pane_id,
            ExternalEditTarget::ContextDocument {
                document_id: document.id,
                project: target_project,
                expected_revision,
            },
            original_content.clone(),
            original_content,
            true,
        )?;
        self.sync_tracked_pty_sizes()?;
        Ok(())
    }

    /// Applies or retains one normal durable completion through its CAS owner.
    pub(super) fn settle_durable_external_edit(
        &mut self,
        completion: &ExternalEditorCompletion,
    ) -> Result<DurableExternalEditSettlement> {
        if !matches!(
            completion.target,
            ExternalEditTarget::IssueBody { .. }
                | ExternalEditTarget::IssueNotes { .. }
                | ExternalEditTarget::MemoryContent { .. }
                | ExternalEditTarget::ContextDocument { .. }
        ) {
            return Ok(DurableExternalEditSettlement::Unhandled);
        }
        if completion.recovery_state.is_none() {
            return Ok(DurableExternalEditSettlement::Retained);
        }
        if !completion.apply_on_success
            || completion.exit_code != 0
            || completion.validated_content.is_none()
        {
            return Ok(DurableExternalEditSettlement::Retained);
        }
        let content = completion
            .validated_content
            .as_deref()
            .expect("validated durable completion content was checked above");
        self.apply_durable_external_edit_target(&completion.pane_id, &completion.target, content)
    }

    /// Applies a validated retained draft to its exact durable target.
    pub(super) fn apply_durable_external_edit_target(
        &mut self,
        pane_id: &str,
        target: &ExternalEditTarget,
        content: &str,
    ) -> Result<DurableExternalEditSettlement> {
        match target {
            ExternalEditTarget::IssueBody {
                project,
                issue_id,
                expected_revision,
            }
            | ExternalEditTarget::IssueNotes {
                project,
                issue_id,
                expected_revision,
            } => {
                let (store, current_project) = self.external_editor_issue_store(pane_id)?;
                if current_project != *project {
                    return Ok(DurableExternalEditSettlement::Conflicted);
                }
                let field = if matches!(target, ExternalEditTarget::IssueBody { .. }) {
                    IssueTextField::Body
                } else {
                    IssueTextField::Notes
                };
                let content = (!content.is_empty()).then(|| content.to_string());
                match store.compare_and_swap_issue_text(
                    project,
                    issue_id,
                    field,
                    expected_revision,
                    content,
                    current_unix_seconds().max(1),
                )? {
                    CompareAndSwapIssueTextResult::Updated(_) => {
                        self.invalidate_agent_prompt_selector_extra_candidates();
                        Ok(DurableExternalEditSettlement::Applied)
                    }
                    CompareAndSwapIssueTextResult::Stale { .. }
                    | CompareAndSwapIssueTextResult::Deleted => {
                        Ok(DurableExternalEditSettlement::Conflicted)
                    }
                }
            }
            ExternalEditTarget::MemoryContent {
                memory_id,
                expected_revision,
            } => {
                let store = self.external_editor_memory_store()?;
                match store.compare_and_swap_content(
                    memory_id,
                    expected_revision,
                    content,
                    current_unix_seconds().max(1),
                )? {
                    CompareAndSwapMemoryContentResult::Updated(record) => {
                        self.session_memory_mut().upsert(*record)?;
                        Ok(DurableExternalEditSettlement::Applied)
                    }
                    CompareAndSwapMemoryContentResult::Stale { .. }
                    | CompareAndSwapMemoryContentResult::Deleted => {
                        Ok(DurableExternalEditSettlement::Conflicted)
                    }
                }
            }
            ExternalEditTarget::ContextDocument {
                document_id,
                project,
                expected_revision,
            } => {
                let config_root = self.external_editor_config_root()?;
                let current_project = self.context_document_project_for_pane(pane_id, &config_root);
                if project
                    .as_ref()
                    .is_some_and(|expected_project| expected_project != &current_project)
                {
                    return Ok(DurableExternalEditSettlement::Conflicted);
                }
                let store = ContextDocumentStore::under_config_root(&config_root);
                let Some(document) = store.inspect(document_id)? else {
                    return Ok(DurableExternalEditSettlement::Conflicted);
                };
                if !document.visible_to_project(&current_project) {
                    return Ok(DurableExternalEditSettlement::Conflicted);
                }
                match store.compare_and_swap_content(
                    document_id,
                    expected_revision,
                    content.to_string(),
                    current_unix_seconds().max(1),
                )? {
                    CompareAndSwapContextDocumentResult::Updated(_) => {
                        Ok(DurableExternalEditSettlement::Applied)
                    }
                    CompareAndSwapContextDocumentResult::Stale { .. }
                    | CompareAndSwapContextDocumentResult::Deleted => {
                        Ok(DurableExternalEditSettlement::Conflicted)
                    }
                }
            }
            _ => Ok(DurableExternalEditSettlement::Unhandled),
        }
    }

    /// Reopens a durable recovery in a fresh restore-only editor session.
    pub(super) fn reopen_durable_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        target: ExternalEditTarget,
        retained_content: String,
    ) -> Result<()> {
        self.start_external_editor_session(
            primary_client_id,
            pane_id,
            target,
            retained_content.clone(),
            retained_content,
            false,
        )?;
        self.sync_tracked_pty_sizes()?;
        Ok(())
    }

    fn external_editor_issue_store(&self, pane_id: &str) -> Result<(IssueStore, String)> {
        let config_root = self.external_editor_config_root()?;
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .unwrap_or_else(|| config_root.clone());
        let project = crate::storage::issues::project_key_for_working_directory(working_directory);
        let store = IssueStore::from_database_path(runtime_issue_database_path(self, &config_root));
        Ok((store, project))
    }

    fn external_editor_memory_store(&self) -> Result<PersistentMemoryStore> {
        if !self.runtime_persistent_memory_enabled() {
            return Err(MezError::invalid_state(
                "external memory editing requires persistent memory to be enabled",
            ));
        }
        Ok(PersistentMemoryStore::under_config_root(
            self.external_editor_config_root()?,
        ))
    }

    fn external_editor_config_root(&self) -> Result<std::path::PathBuf> {
        self.integration
            .config_root()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| MezError::config("external editing requires a configured config root"))
    }
}
