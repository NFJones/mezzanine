//! Agent-shell commands for persisted user-owned context documents.

use super::{
    AgentShellCommandOutcome, MezError, Result, RuntimeSessionService, current_unix_seconds,
    json_escape, parse_slash_command,
};
use crate::storage::context_documents::{
    ContextDocument, ContextDocumentScope, ContextDocumentStore,
};

impl RuntimeSessionService {
    /// Executes `/context-doc` CRUD, inclusion, and external-edit operations.
    pub(super) fn execute_agent_shell_context_document_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("context-doc command must be a slash command"))?;
        let Some(config_root) = self.integration.config_root().map(ToOwned::to_owned) else {
            return Err(MezError::config(
                "context-doc requires a configured Mezzanine config root",
            ));
        };
        let store = ContextDocumentStore::under_config_root(&config_root);
        let project = self.context_document_project_for_pane(pane_id, &config_root);
        let arguments = shlex::split(&invocation.args).ok_or_else(|| {
            MezError::invalid_args("context-doc arguments contain an unterminated quote")
        })?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        match arguments.first().map(String::as_str) {
            Some("create") => {
                let (scope, title, enabled) = parse_create_arguments(&arguments[1..], &project)?;
                let document = store.create(
                    scope,
                    title,
                    String::new(),
                    enabled,
                    current_unix_seconds().max(1),
                )?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "context-doc".to_string(),
                    body: format!(
                        "context document created id={} enabled={} title={}",
                        document.id,
                        document.enabled,
                        json_escape(&document.title),
                    ),
                    visibility,
                })
            }
            Some("list") | None => {
                let records = store
                    .list()?
                    .into_iter()
                    .filter(|document| document.visible_to_project(&project))
                    .collect::<Vec<_>>();
                Ok(AgentShellCommandOutcome::Display {
                    command: "context-doc".to_string(),
                    body: context_document_list_display(&records),
                })
            }
            Some("show") => {
                let id = required_single_id(&arguments, "show")?;
                let document = authorized_document(&store, id, &project)?;
                Ok(AgentShellCommandOutcome::Display {
                    command: "context-doc".to_string(),
                    body: format!(
                        "context document found=true\nid={}\nscope={}\nenabled={}\ntitle={}\ncontent={}",
                        document.id,
                        scope_display(&document.scope),
                        document.enabled,
                        json_escape(&document.title),
                        json_escape(&document.content),
                    ),
                })
            }
            Some("edit") => {
                let id = required_single_id(&arguments, "edit")?;
                authorized_document(&store, id, &project)?;
                self.start_context_document_external_edit(primary_client_id, pane_id, id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "context-doc".to_string(),
                    body: format!("context document edit id={id} editor_started=true"),
                    visibility,
                })
            }
            Some("enable" | "disable") => {
                let operation = arguments[0].as_str();
                let id = required_single_id(&arguments, operation)?;
                authorized_document(&store, id, &project)?;
                let enabled = operation == "enable";
                let updated = store
                    .set_enabled(id, enabled, current_unix_seconds().max(1))?
                    .ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "context document not found",
                        )
                    })?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "context-doc".to_string(),
                    body: format!(
                        "context document inclusion id={} enabled={} changed=true",
                        updated.id, updated.enabled
                    ),
                    visibility,
                })
            }
            Some("delete") => {
                let id = required_single_id(&arguments, "delete")?;
                authorized_document(&store, id, &project)?;
                let deleted = store.delete(id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "context-doc".to_string(),
                    body: format!("context document delete id={id} deleted={deleted}"),
                    visibility,
                })
            }
            _ => Err(MezError::invalid_args(
                "context-doc expects create, list, show <id>, edit <id>, enable <id>, disable <id>, or delete <id>",
            )),
        }
    }

    pub(in crate::runtime) fn context_document_project_for_pane(
        &self,
        pane_id: &str,
        config_root: &std::path::Path,
    ) -> String {
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .unwrap_or_else(|| config_root.to_path_buf());
        crate::security::project::discover_project_root(&working_directory)
            .to_string_lossy()
            .into_owned()
    }
}

fn parse_create_arguments(
    arguments: &[String],
    project: &str,
) -> Result<(ContextDocumentScope, String, bool)> {
    let mut scope = None;
    let mut title = None;
    let mut enabled = false;
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--scope" => {
                index = index.saturating_add(1);
                scope = Some(match arguments.get(index).map(String::as_str) {
                    Some("global") => ContextDocumentScope::Global,
                    Some("project") => ContextDocumentScope::Project {
                        root: project.to_string(),
                    },
                    _ => {
                        return Err(MezError::invalid_args(
                            "context-doc create --scope expects global or project",
                        ));
                    }
                });
            }
            "--title" => {
                index = index.saturating_add(1);
                title = arguments.get(index).cloned();
            }
            "--disabled" => enabled = false,
            _ => {
                return Err(MezError::invalid_args(
                    "context-doc create accepts --scope, --title, and --disabled",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok((
        scope.ok_or_else(|| MezError::invalid_args("context-doc create requires --scope"))?,
        title.ok_or_else(|| MezError::invalid_args("context-doc create requires --title"))?,
        enabled,
    ))
}

fn required_single_id<'a>(arguments: &'a [String], operation: &str) -> Result<&'a str> {
    if arguments.len() != 2 {
        return Err(MezError::invalid_args(format!(
            "context-doc {operation} expects one document id"
        )));
    }
    Ok(&arguments[1])
}

fn authorized_document(
    store: &ContextDocumentStore,
    id: &str,
    project: &str,
) -> Result<ContextDocument> {
    let document = store.inspect(id)?.ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            "context document not found",
        )
    })?;
    if !document.visible_to_project(project) {
        return Err(MezError::forbidden(
            "context document does not belong to the active project",
        ));
    }
    Ok(document)
}

fn context_document_list_display(documents: &[ContextDocument]) -> String {
    let mut lines = vec![format!("context documents count={}", documents.len())];
    for document in documents {
        lines.push(format!(
            "id={} scope={} enabled={} title={} updated_at_unix_seconds={}",
            document.id,
            scope_display(&document.scope),
            document.enabled,
            json_escape(&document.title),
            document.updated_at_unix_seconds,
        ));
    }
    lines.join("\n")
}

fn scope_display(scope: &ContextDocumentScope) -> String {
    match scope {
        ContextDocumentScope::Global => "global".to_string(),
        ContextDocumentScope::Project { root } => format!("project:{}", json_escape(root)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_document_create_parser_keeps_scope_and_inclusion_typed() {
        let (scope, title, enabled) = parse_create_arguments(
            &shlex::split("--scope project --title 'Runbook'").unwrap(),
            "/repo",
        )
        .unwrap();
        assert_eq!(
            scope,
            ContextDocumentScope::Project {
                root: "/repo".to_string()
            }
        );
        assert_eq!(title, "Runbook");
        assert!(!enabled);
    }
}
