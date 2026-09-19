//! Deferred execution for runtime slash commands that read stores or files.
//!
//! Prompt slash commands are applied inside the attached-client actor request, so
//! a command that walks a store or the filesystem parks the serialized actor for
//! the whole read while every other client keystroke, pane output chunk, and
//! provider event waits behind it. This module owns the off-actor half of that
//! split: the actor captures owned inputs while it still owns the pane, a worker
//! renders the outcome, and the actor applies it through the same display path
//! the inline lane used, so presentation stays byte-identical.
//!
//! Families move here one at a time. A command that is not listed in
//! [`RUNTIME_AGENT_OFF_ACTOR_COMMANDS`] keeps executing inline even when the
//! disposition classifier calls it deferred, so the classification stays the
//! contract this executor consumes instead of a switch for every deferred name
//! at once.

use super::{
    AgentShellCommandOutcome, AgentShellVisibility, MezError, Result, RuntimeSessionService,
    lists::{runtime_agent_macro_catalog_body, runtime_agent_skill_catalog_body},
    runtime_agent_shell_command_response_json,
    shell::agent_shell_invalid_command_response_json,
};
use crate::runtime::{
    RuntimeAgentCommandAsyncOutcome, RuntimeAgentCommandAsyncWork, RuntimeAgentCommandDispatch,
    RuntimeAgentCommandLifecyclePhase, RuntimeAgentCommandPrepared,
    runtime_agent_shell_deferred_command_response_json,
};

/// Slash commands whose store or filesystem reads already run off the actor.
///
/// Every entry must also be classified
/// [`super::disposition::RuntimeAgentSlashCommandDisposition::Deferred`], which
/// the guard test below pins.
pub(crate) const RUNTIME_AGENT_OFF_ACTOR_COMMANDS: &[&str] = &[
    "list-skills",
    "list-macros",
    "auth-status",
    "issue",
    "show-issues",
    "show-memories",
    "show-context",
    "context-doc",
    "sync-builtin-skills",
    "resume",
    "list-modified-files",
    "show-approvals",
    "list-personalities",
];

/// Prepared-input family one moved slash command consumes off the actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentCommandFamily {
    /// Catalog displays that walk the configured and project catalogs.
    Catalog,
    /// Provider credential status reads.
    AuthStatus,
    /// Local project issue reads.
    IssueStore,
    /// Local issue browser reads.
    IssueBrowser,
    /// Persistent-memory browser reads.
    MemoryBrowser,
    /// Pane transcript reads backing the context browser.
    ContextBrowser,
    /// Context-document reads.
    ContextDocument,
    /// Managed built-in skill syncs.
    BuiltinSkillSync,
    /// Saved-session catalog reads for the `/resume` picker.
    SavedSessionsBrowser,
    /// Pane-local modified-file summaries.
    ModifiedFiles,
    /// Pending-approval browser reads.
    ApprovalsBrowser,
    /// Personality-table browser reads.
    PersonalitiesBrowser,
}

/// Returns the prepared-input family for one moved command.
///
/// The claim uses this as the single authority for which commands may run off the
/// actor, and the guard test pins every [`RUNTIME_AGENT_OFF_ACTOR_COMMANDS`]
/// entry to a family, so a name cannot be added to the dispatcher list without
/// prepared inputs and then be acknowledged and silently dropped at claim time.
pub(crate) fn off_actor_command_family(command: &str) -> Option<RuntimeAgentCommandFamily> {
    match command {
        "list-skills" | "list-macros" => Some(RuntimeAgentCommandFamily::Catalog),
        "auth-status" => Some(RuntimeAgentCommandFamily::AuthStatus),
        "issue" => Some(RuntimeAgentCommandFamily::IssueStore),
        "show-issues" => Some(RuntimeAgentCommandFamily::IssueBrowser),
        "show-memories" => Some(RuntimeAgentCommandFamily::MemoryBrowser),
        "show-context" => Some(RuntimeAgentCommandFamily::ContextBrowser),
        "context-doc" => Some(RuntimeAgentCommandFamily::ContextDocument),
        "sync-builtin-skills" => Some(RuntimeAgentCommandFamily::BuiltinSkillSync),
        "resume" => Some(RuntimeAgentCommandFamily::SavedSessionsBrowser),
        "list-modified-files" => Some(RuntimeAgentCommandFamily::ModifiedFiles),
        "show-approvals" => Some(RuntimeAgentCommandFamily::ApprovalsBrowser),
        "list-personalities" => Some(RuntimeAgentCommandFamily::PersonalitiesBrowser),
        _ => None,
    }
}

impl RuntimeSessionService {
    /// Starts one actor-owned claim for a deferred slash command.
    ///
    /// The generation is stamped by the actor that dispatches the work and
    /// compared when the outcome settles, so resubmitting the same command while
    /// an earlier attempt is still in flight cannot apply a stale display.
    pub(crate) fn begin_agent_command_claim(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<u64> {
        self.agent
            .begin_agent_command_claim(pane_id, conversation_id)
    }

    /// Reports whether the pane prompt is owned by a deferred command.
    pub(crate) fn agent_command_is_active(&self, pane_id: &str) -> bool {
        self.agent.agent_command_is_active(pane_id)
    }

    /// Reports whether one prompt command may run through the deferred lane.
    ///
    /// The dispatcher owns this decision because part of it depends on live
    /// service state: an `/issue` read needs the store enabled and a resolvable
    /// database, and its mutating forms stay inline because they also invalidate
    /// prompt selector candidates on the actor. A command this refuses keeps the
    /// inline path, so the user still receives a synchronous body instead of an
    /// acknowledgement that nothing ever settles.
    pub(crate) fn should_defer_agent_shell_command(&self, command: &str, input: &str) -> bool {
        if !RUNTIME_AGENT_OFF_ACTOR_COMMANDS.contains(&command) {
            return false;
        }
        // The disposition classifier is the contract this executor consumes, so a
        // moved command must be a known deferred command: membership rejects an
        // unclassified name the classifier would default to deferred, and the
        // classifier check rejects a name a later edit also pinned inline. Either
        // disagreement would silently change where a read runs.
        if !super::disposition::RUNTIME_AGENT_DEFERRED_SLASH_COMMANDS.contains(&command) {
            return false;
        }
        if super::disposition::runtime_agent_slash_command_disposition(command)
            != super::disposition::RuntimeAgentSlashCommandDisposition::Deferred
        {
            return false;
        }
        match command {
            "issue" => {
                super::issues::runtime_issues_enabled(self)
                    && self.integration.config_root().is_some()
                    && super::issues::runtime_agent_issue_args_are_read_only(input)
            }
            "show-issues" => {
                super::issues::runtime_issues_enabled(self)
                    && self.integration.config_root().is_some()
                    && super::show_records::show_issues_args_are_browser_form(input)
            }
            "show-memories" => {
                self.runtime_persistent_memory_enabled()
                    && self.integration.config_root().is_some()
                    && super::show_records::show_memories_args_are_browser_form(input)
            }
            "show-context" => {
                self.persistence.transcript_store().is_some()
                    && super::show_records::show_context_args_are_browser_form(input)
            }
            "context-doc" => {
                self.integration.config_root().is_some()
                    && super::context_documents::runtime_agent_context_document_args_are_read_only(
                        input,
                    )
            }
            "sync-builtin-skills" => self.integration.config_root().is_some(),
            "resume" => {
                super::resume::runtime_agent_resume_args_are_picker(input)
                    && self.persistence.transcript_store().is_some()
            }
            "list-personalities" => {
                super::preferences::runtime_agent_list_personalities_args_are_empty(input)
            }
            _ => true,
        }
    }

    /// Queues one deferred slash command for off-actor execution.
    ///
    /// Acceptance performs only bounded ownership checks and queues worker work.
    /// Project-layer discovery and file reads must not delay the terminal-step
    /// acknowledgement; prompt admission and explicit configuration refreshes
    /// remain responsible for installing current project configuration.
    pub(crate) fn dispatch_deferred_agent_shell_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        command: &str,
        input: &str,
    ) -> Result<String> {
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        let claim_generation = self.begin_agent_command_claim(pane_id, &conversation_id)?;
        self.presentation
            .push_pending_deferred_agent_command(RuntimeAgentCommandDispatch {
                primary_client_id: primary_client_id.clone(),
                pane_id: pane_id.to_string(),
                conversation_id,
                command: command.to_string(),
                input: input.to_string(),
                claim_generation,
            });
        Ok(runtime_agent_shell_deferred_command_response_json(
            pane_id, input, command,
        ))
    }

    /// Captures the owned inputs one deferred slash command may read.
    ///
    /// The claim re-checks the same guards the inline path applied inside the
    /// actor request (attached primary, visible agent prompt, current claim
    /// generation) and then copies exactly what the off-actor execution needs,
    /// so the worker never reaches into live service state. A claim that no
    /// longer matches answers `None` and the worker drops the dispatch.
    pub(crate) fn claim_agent_command_work(
        &mut self,
        _primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        command: &str,
        input: &str,
        claim_generation: u64,
        conversation_id: &str,
    ) -> Result<Option<RuntimeAgentCommandAsyncWork>> {
        let current_conversation = self
            .agent_shell_store()
            .get(pane_id)
            .filter(|session| session.visibility == AgentShellVisibility::Visible)
            .map(|session| session.session_id.as_str());
        if current_conversation != Some(conversation_id) {
            self.agent
                .cancel_matching_agent_command(pane_id, conversation_id, claim_generation);
            return Ok(None);
        }
        if !self.should_defer_agent_shell_command(command, input) {
            return self.fail_agent_command_before_claim(
                pane_id,
                conversation_id,
                command,
                input,
                claim_generation,
                "deferred command is no longer available",
            );
        }
        let Some(family) = off_actor_command_family(command) else {
            return self.fail_agent_command_before_claim(
                pane_id,
                conversation_id,
                command,
                input,
                claim_generation,
                "deferred command has no worker executor",
            );
        };
        // Each family names exactly what the worker may read; a command with no
        // family has no off-actor executor yet and keeps executing inline.
        let prepared = match family {
            RuntimeAgentCommandFamily::Catalog => RuntimeAgentCommandPrepared::Catalog {
                config_root: self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf),
                project_root: self.trusted_skill_project_root_for_pane(pane_id),
            },
            RuntimeAgentCommandFamily::AuthStatus => RuntimeAgentCommandPrepared::AuthStatus {
                providers: self
                    .provider_registry()
                    .providers()
                    .keys()
                    .cloned()
                    .collect(),
                auth_store: self.auth_store().cloned(),
            },
            RuntimeAgentCommandFamily::IssueStore => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf)
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "configured issue store is no longer available",
                    );
                };
                let working_directory = self
                    .pane_current_working_directory(pane_id)
                    .unwrap_or_else(|| config_root.clone());
                RuntimeAgentCommandPrepared::IssueStore {
                    database_path: super::issues::runtime_issue_database_path(self, &config_root),
                    project: crate::storage::issues::project_key_for_working_directory(
                        working_directory,
                    ),
                }
            }
            RuntimeAgentCommandFamily::IssueBrowser => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf)
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "configured issue browser is no longer available",
                    );
                };
                let working_directory = self
                    .pane_current_working_directory(pane_id)
                    .unwrap_or_else(|| config_root.clone());
                RuntimeAgentCommandPrepared::IssueBrowser {
                    database_path: super::issues::runtime_issue_database_path(self, &config_root),
                    project: crate::storage::issues::project_key_for_working_directory(
                        working_directory,
                    ),
                }
            }
            RuntimeAgentCommandFamily::MemoryBrowser => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf)
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "configured memory browser is no longer available",
                    );
                };
                RuntimeAgentCommandPrepared::MemoryBrowser {
                    config_root,
                    pane_scope: self.runtime_remember_scope_for_pane(pane_id),
                }
            }
            RuntimeAgentCommandFamily::ContextBrowser => {
                let Some(store) = self.persistence.cloned_transcript_store() else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "transcript store is no longer available",
                    );
                };
                let Some(conversation_id) = self
                    .agent_shell_store()
                    .get(pane_id)
                    .map(|session| session.session_id.clone())
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "owning conversation is no longer available",
                    );
                };
                RuntimeAgentCommandPrepared::ContextBrowser {
                    store,
                    conversation_id,
                    pane_id: pane_id.to_string(),
                }
            }
            RuntimeAgentCommandFamily::ContextDocument => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf)
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "configured context document store is no longer available",
                    );
                };
                let project = self.context_document_project_for_pane(pane_id, &config_root);
                RuntimeAgentCommandPrepared::ContextDocument {
                    config_root,
                    project,
                }
            }
            RuntimeAgentCommandFamily::BuiltinSkillSync => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf)
                else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "configured skill store is no longer available",
                    );
                };
                RuntimeAgentCommandPrepared::BuiltinSkillSync { config_root }
            }
            RuntimeAgentCommandFamily::SavedSessionsBrowser => {
                let Some(store) = self.persistence.cloned_transcript_store() else {
                    return self.fail_agent_command_before_claim(
                        pane_id,
                        conversation_id,
                        command,
                        input,
                        claim_generation,
                        "saved-session store is no longer available",
                    );
                };
                let directory = self
                    .pane_current_working_directory(pane_id)
                    .map(|path| path.to_string_lossy().into_owned());
                RuntimeAgentCommandPrepared::SavedSessionsBrowser {
                    store,
                    directory,
                    limit: self.saved_session_page_limit(),
                    prompt_width: self.saved_session_prompt_width(),
                    title_policy: self.agent_session_title_policy(),
                }
            }
            RuntimeAgentCommandFamily::ModifiedFiles => {
                RuntimeAgentCommandPrepared::ModifiedFiles {
                    files: self.retained_agent_modified_files(pane_id).cloned(),
                }
            }
            RuntimeAgentCommandFamily::ApprovalsBrowser => {
                RuntimeAgentCommandPrepared::ApprovalsBrowser {
                    approvals: self
                        .blocked_approvals()
                        .pending()
                        .into_iter()
                        .cloned()
                        .collect(),
                }
            }
            RuntimeAgentCommandFamily::PersonalitiesBrowser => {
                let pane_selection = self
                    .integration
                    .agent_personality_selections()
                    .get(pane_id)
                    .map(String::as_str)
                    .filter(|profile_id| {
                        self.integration
                            .agent_personality_profiles()
                            .contains_key(*profile_id)
                    })
                    .map(str::to_string);
                let selected = self
                    .agent_selected_personality_profile_id(pane_id)
                    .map(str::to_string);
                let profiles = self
                    .integration
                    .agent_personality_profiles()
                    .iter()
                    .map(|(id, profile)| (id.clone(), profile.clone()))
                    .collect();
                RuntimeAgentCommandPrepared::PersonalitiesBrowser {
                    profiles,
                    selected,
                    pane_selection,
                }
            }
        };
        if !self
            .agent
            .claim_agent_command(pane_id, conversation_id, claim_generation)
        {
            return Ok(None);
        }
        Ok(Some(RuntimeAgentCommandAsyncWork {
            pane_id: pane_id.to_string(),
            conversation_id: conversation_id.to_string(),
            command: command.to_string(),
            input: input.to_string(),
            claim_generation,
            prepared,
        }))
    }

    /// Settles an accepted command that became unusable before worker claim.
    fn fail_agent_command_before_claim(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        command: &str,
        input: &str,
        command_id: u64,
        message: &str,
    ) -> Result<Option<RuntimeAgentCommandAsyncWork>> {
        if !self
            .agent
            .claim_agent_command(pane_id, conversation_id, command_id)
        {
            return Ok(None);
        }
        let body = agent_shell_invalid_command_response_json(
            pane_id,
            input,
            &MezError::invalid_state(format!("{command}: {message}")),
        );
        let result = self.apply_deferred_agent_shell_response_body(pane_id, &body);
        self.agent.settle_agent_command(
            pane_id,
            conversation_id,
            command_id,
            RuntimeAgentCommandLifecyclePhase::Failed,
        );
        result?;
        Ok(None)
    }

    /// Executes one claimed deferred slash command off actor ownership.
    ///
    /// The function is deliberately static: the worker must not be able to reach
    /// live service state, so everything it may read arrives in `work`. It
    /// returns the same response body the inline lane produced for the same
    /// catalog, which is what makes the completion presentation byte-identical.
    pub(crate) fn execute_deferred_agent_command(
        work: &RuntimeAgentCommandAsyncWork,
    ) -> RuntimeAgentCommandAsyncOutcome {
        let body = match &work.prepared {
            RuntimeAgentCommandPrepared::Catalog {
                config_root,
                project_root,
            } => match work.command.as_str() {
                "list-skills" => runtime_agent_skill_catalog_body(
                    &crate::integrations::skills::discover_skill_catalog(
                        config_root.as_deref(),
                        project_root.as_deref(),
                    ),
                ),
                "list-macros" => runtime_agent_macro_catalog_body(
                    &crate::integrations::macros::discover_macro_catalog(
                        config_root.as_deref(),
                        project_root.as_deref(),
                    ),
                ),
                other => {
                    return RuntimeAgentCommandAsyncOutcome::Failed {
                        message: format!(
                            "deferred catalog command `{other}` has no off-actor executor"
                        ),
                        kind: crate::error::MezErrorKind::InvalidState,
                    };
                }
            },
            RuntimeAgentCommandPrepared::AuthStatus {
                providers,
                auth_store,
            } => {
                match super::status::runtime_agent_auth_status_body(providers, auth_store.as_ref())
                {
                    Ok(body) => body,
                    Err(error) => {
                        return RuntimeAgentCommandAsyncOutcome::Failed {
                            message: error.message().to_string(),
                            kind: error.kind(),
                        };
                    }
                }
            }
            RuntimeAgentCommandPrepared::IssueStore {
                database_path,
                project,
            } => {
                match super::issues::runtime_agent_issue_read_body(
                    database_path.clone(),
                    project,
                    &work.input,
                ) {
                    Ok(body) => body,
                    Err(error) => {
                        return RuntimeAgentCommandAsyncOutcome::Failed {
                            message: error.message().to_string(),
                            kind: error.kind(),
                        };
                    }
                }
            }
            RuntimeAgentCommandPrepared::IssueBrowser {
                database_path,
                project,
            } => {
                return match super::show_records::read_issue_browser(
                    database_path.clone(),
                    project.clone(),
                    &work.input,
                ) {
                    Ok(read) => {
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "show-issues".to_string(),
                            body: read.markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "show-issues".to_string(),
                            browser: Box::new(read.browser),
                            source: read.source,
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::ContextBrowser {
                store,
                conversation_id,
                pane_id,
            } => {
                return match super::show_records::read_context_browser_for_command(
                    store,
                    conversation_id,
                    pane_id,
                    &work.input,
                ) {
                    Ok(read) => {
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "show-context".to_string(),
                            body: read.markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "show-context".to_string(),
                            browser: Box::new(read.browser),
                            source: read.source,
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::MemoryBrowser {
                config_root,
                pane_scope,
            } => {
                return match super::show_records::read_memory_browser(
                    config_root.clone(),
                    pane_scope.clone(),
                    &work.input,
                ) {
                    Ok(read) => {
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "show-memories".to_string(),
                            body: read.markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "show-memories".to_string(),
                            browser: Box::new(read.browser),
                            source: read.source,
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::ContextDocument {
                config_root,
                project,
            } => {
                match super::context_documents::runtime_agent_context_document_read_body(
                    config_root,
                    project,
                    &work.input,
                ) {
                    Ok(body) => body,
                    Err(error) => {
                        return RuntimeAgentCommandAsyncOutcome::Failed {
                            message: error.message().to_string(),
                            kind: error.kind(),
                        };
                    }
                }
            }
            RuntimeAgentCommandPrepared::BuiltinSkillSync { config_root } => {
                return match super::lists::runtime_agent_builtin_skill_sync_body(config_root) {
                    Ok(body) => {
                        let outcome = AgentShellCommandOutcome::Mutated {
                            command: "sync-builtin-skills".to_string(),
                            body,
                            visibility: AgentShellVisibility::Visible,
                        };
                        RuntimeAgentCommandAsyncOutcome::Response {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::SavedSessionsBrowser {
                store,
                directory,
                limit,
                prompt_width,
                title_policy,
            } => {
                return match super::resume::runtime_agent_saved_sessions_browser(
                    store,
                    directory.as_deref(),
                    crate::storage::transcript::SavedSessionLifecycleFilter::Active,
                    false,
                    None,
                    None,
                    *limit,
                    *prompt_width,
                    *title_policy,
                ) {
                    Ok(browser) => {
                        let page = browser.render_page();
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "resume".to_string(),
                            body: page.raw_markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "resume".to_string(),
                            browser: Box::new(browser),
                            source: Some(
                                super::resume::runtime_agent_saved_sessions_overlay_source(
                                    directory.clone(),
                                    *limit,
                                ),
                            ),
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::ModifiedFiles { files } => {
                super::lists::runtime_agent_modified_files_body(files.as_ref())
            }
            RuntimeAgentCommandPrepared::ApprovalsBrowser { approvals } => {
                return match super::show_records::runtime_agent_approval_args(&work.input).and_then(
                    |active| {
                        super::show_records::runtime_agent_approval_browser(
                            approvals.clone(),
                            active.as_deref(),
                        )
                    },
                ) {
                    Ok(browser) => {
                        let page = browser.render_page();
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "show-approvals".to_string(),
                            body: page.raw_markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "show-approvals".to_string(),
                            browser: Box::new(browser),
                            source: Some(
                                crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::Approvals,
                            ),
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
            RuntimeAgentCommandPrepared::PersonalitiesBrowser {
                profiles,
                selected,
                pane_selection,
            } => {
                return match super::preferences::runtime_agent_personality_browser(
                    profiles,
                    selected.as_deref(),
                    pane_selection.as_deref(),
                ) {
                    Ok(browser) => {
                        let page = browser.render_page();
                        let outcome = AgentShellCommandOutcome::Display {
                            command: "list-personalities".to_string(),
                            body: page.raw_markdown,
                        };
                        RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                            body: runtime_agent_shell_command_response_json(
                                &work.pane_id,
                                &work.input,
                                Some(&outcome),
                            ),
                            command: "list-personalities".to_string(),
                            browser: Box::new(browser),
                            source: Some(
                                crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::Personalities {
                                    pane_id: work.pane_id.clone(),
                                },
                            ),
                        }
                    }
                    Err(error) => RuntimeAgentCommandAsyncOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    },
                };
            }
        };
        let outcome = AgentShellCommandOutcome::Display {
            command: work.command.clone(),
            body,
        };
        RuntimeAgentCommandAsyncOutcome::Response {
            body: runtime_agent_shell_command_response_json(
                &work.pane_id,
                &work.input,
                Some(&outcome),
            ),
        }
    }

    /// Applies one settled deferred slash command outcome on the actor.
    ///
    /// Stale outcomes are dropped: the pane's current claim generation must still
    /// match the dispatched one, so an earlier attempt cannot overwrite a display
    /// the user asked for again. A failure is reported through the same
    /// invalid-command response path the inline lane used.
    ///
    /// A pane that was hidden or closed while the worker ran also drops its
    /// display: the prompt that asked for it no longer exists, and the claim
    /// refuses work for a pane whose agent prompt is not visible, matching the
    /// submit-time guard the inline lane applied.
    pub(crate) fn complete_agent_command_work(
        &mut self,
        work: &RuntimeAgentCommandAsyncWork,
        outcome: RuntimeAgentCommandAsyncOutcome,
    ) -> Result<bool> {
        let current_conversation = self
            .agent_shell_store()
            .get(&work.pane_id)
            .map(|session| session.session_id.as_str());
        if current_conversation != Some(work.conversation_id.as_str()) {
            self.agent.cancel_matching_agent_command(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
            );
            return Ok(false);
        }
        let is_failure = matches!(outcome, RuntimeAgentCommandAsyncOutcome::Failed { .. });
        let claimed = self.agent.agent_command_is_claimed(
            &work.pane_id,
            &work.conversation_id,
            work.claim_generation,
        );
        if !claimed {
            return Ok(false);
        }
        let body = match outcome {
            RuntimeAgentCommandAsyncOutcome::Response { body } => body,
            RuntimeAgentCommandAsyncOutcome::Failed { message, kind } => {
                RuntimeSessionService::deferred_agent_command_failure_body(work, &message, kind)
            }
            RuntimeAgentCommandAsyncOutcome::RecordBrowser {
                body,
                command,
                browser,
                source,
            } => {
                // The inline lane installed the browser overlay before it returned
                // the page body, so the deferred lane installs it with the same
                // call and then applies the body through the same display path.
                self.register_pending_record_browser_overlay(
                    &work.pane_id,
                    &command,
                    *browser,
                    source,
                );
                body
            }
        };
        if let Err(error) = self.apply_deferred_agent_shell_response_body(&work.pane_id, &body) {
            self.agent.settle_agent_command(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
                RuntimeAgentCommandLifecyclePhase::Failed,
            );
            return Err(error);
        }
        let phase = if is_failure {
            RuntimeAgentCommandLifecyclePhase::Failed
        } else {
            RuntimeAgentCommandLifecyclePhase::Completed
        };
        if !self.agent.settle_agent_command(
            &work.pane_id,
            &work.conversation_id,
            work.claim_generation,
            phase,
        ) {
            return Ok(false);
        }
        Ok(true)
    }

    /// Builds the invalid-command response body for one deferred failure.
    ///
    /// The inline lane reports the error kind it received, so the deferred lane
    /// carries that kind through the outcome and renders the same code instead of
    /// normalizing every failure to `invalid_state`.
    pub(crate) fn deferred_agent_command_failure_body(
        work: &RuntimeAgentCommandAsyncWork,
        message: &str,
        kind: crate::error::MezErrorKind,
    ) -> String {
        agent_shell_invalid_command_response_json(
            &work.pane_id,
            &work.input,
            &MezError::new(kind, message),
        )
    }

    /// Runs every queued deferred slash command through the worker lane.
    ///
    /// The production worker claims, executes, and settles through actor
    /// requests; focused tests drive the same three steps in-process so they can
    /// assert the applied body without standing up the async runtime client.
    #[cfg(test)]
    pub(crate) fn run_pending_deferred_agent_command_for_tests(
        &mut self,
    ) -> Result<Option<String>> {
        let mut last_body = None;
        for dispatch in self.take_pending_deferred_agent_commands() {
            let Some(work) = self.claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id,
            )?
            else {
                continue;
            };
            let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
            let body = match &outcome {
                RuntimeAgentCommandAsyncOutcome::Response { body } => body.clone(),
                RuntimeAgentCommandAsyncOutcome::RecordBrowser { body, .. } => body.clone(),
                RuntimeAgentCommandAsyncOutcome::Failed { message, kind } => {
                    RuntimeSessionService::deferred_agent_command_failure_body(
                        &work, message, *kind,
                    )
                }
            };
            if !self.complete_agent_command_work(&work, outcome)? {
                continue;
            }
            last_body = Some(body);
        }
        Ok(last_body)
    }

    /// Returns the retained lifecycle phase for one pane in focused tests.
    #[cfg(test)]
    pub(crate) fn agent_command_lifecycle_phase_for_tests(
        &self,
        pane_id: &str,
    ) -> Option<RuntimeAgentCommandLifecyclePhase> {
        self.agent.agent_command_lifecycle_phase_for_tests(pane_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::commands::disposition::{
        RuntimeAgentSlashCommandDisposition, runtime_agent_slash_command_disposition,
    };

    /// Verifies every command the executor runs off the actor is also classified
    /// deferred, so the two lists cannot drift apart.
    #[test]
    fn runtime_agent_off_actor_commands_are_classified_deferred() {
        for command in RUNTIME_AGENT_OFF_ACTOR_COMMANDS {
            assert!(
                off_actor_command_family(command).is_some(),
                "`{command}` is dispatched off actor, so it must have prepared inputs"
            );
            assert_eq!(
                runtime_agent_slash_command_disposition(command),
                RuntimeAgentSlashCommandDisposition::Deferred,
                "`{command}` runs off the actor, so the disposition classifier must defer it"
            );
        }
    }

    /// Verifies a deferred failure keeps the error kind it carried.
    ///
    /// The inline lane reports the kind it received from the failing read; the
    /// deferred lane must render the same code instead of normalizing every
    /// failure to `invalid_state`, or the two lanes would disagree about how a
    /// credential or config error is reported.
    #[test]
    fn runtime_agent_deferred_failure_keeps_the_carried_error_kind() {
        let work = RuntimeAgentCommandAsyncWork {
            pane_id: "%1".to_string(),
            conversation_id: "conversation-1".to_string(),
            command: "auth-status".to_string(),
            input: "/auth-status".to_string(),
            claim_generation: 1,
            prepared: RuntimeAgentCommandPrepared::AuthStatus {
                providers: Vec::new(),
                auth_store: None,
            },
        };
        let body = RuntimeSessionService::deferred_agent_command_failure_body(
            &work,
            "credential store is unreadable",
            crate::error::MezErrorKind::Io,
        );
        assert!(
            body.contains("internal_error"),
            "an io failure must keep the code the inline lane reported: {body}"
        );
        assert!(body.contains("credential store is unreadable"), "{body}");
        assert!(
            !body.contains("invalid_state"),
            "the deferred lane must not normalize the error code: {body}"
        );
    }

    /// Verifies the catalog executor renders the same body the inline lane
    /// returned, which is what keeps the deferred presentation byte-identical.
    #[test]
    fn runtime_agent_deferred_catalog_matches_the_inline_body() {
        let catalog = crate::integrations::skills::discover_skill_catalog(None, None);
        let work = RuntimeAgentCommandAsyncWork {
            pane_id: "%1".to_string(),
            conversation_id: "conversation-1".to_string(),
            command: "list-skills".to_string(),
            input: "/list-skills".to_string(),
            claim_generation: 1,
            prepared: RuntimeAgentCommandPrepared::Catalog {
                config_root: None,
                project_root: None,
            },
        };
        let expected = runtime_agent_shell_command_response_json(
            &work.pane_id,
            &work.input,
            Some(&AgentShellCommandOutcome::Display {
                command: "list-skills".to_string(),
                body: runtime_agent_skill_catalog_body(&catalog),
            }),
        );
        let RuntimeAgentCommandAsyncOutcome::Response { body } =
            RuntimeSessionService::execute_deferred_agent_command(&work)
        else {
            panic!("the catalog executor must produce a response body");
        };
        assert_eq!(
            body, expected,
            "the deferred catalog body must match the inline body byte for byte"
        );
    }
}
