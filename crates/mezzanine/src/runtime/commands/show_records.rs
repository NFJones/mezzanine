//! Agent-shell record-browser commands for issues and memories.
//!
//! This module wires the reusable runtime record-browser model to concrete
//! issue and memory stores. It keeps query parsing, record adaptation, and
//! save-to-file behavior close to the live slash-command runtime while leaving
//! the browser state itself backend-agnostic.

use super::{
    AgentShellCommandOutcome, MezError, Result, RuntimeSessionService, json_escape,
    parse_slash_command, runtime_remember_scope_display,
};
use crate::runtime::commands::issues;
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use mez_agent::memory::{
    MemorySearchRequest, kind_name, parse_kind, parse_state, source_name, state_name,
};
use mez_mux::record_browser::{
    RecordBrowser, RecordBrowserFilterChoice, RecordBrowserFilterField, RecordBrowserRecord,
};
use std::{fs, path::PathBuf};

const DEFAULT_SHOW_RECORD_LIMIT: usize = 100;

impl RuntimeSessionService {
    /// Executes `/show-approvals` by projecting the live pending queue into the
    /// shared retained record browser.
    pub(super) fn execute_agent_shell_show_approvals_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("show-approvals command must be a slash command")
        })?;
        let args = slash.args.split_whitespace().collect::<Vec<_>>();
        if args.len() > 1 {
            return Err(MezError::invalid_args(
                "show-approvals accepts at most one approval id",
            ));
        }
        let mut browser = self.approval_record_browser()?;
        if let Some(approval_id) = args.first() {
            if !browser.set_active_record_id(approval_id) {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pending approval was not found",
                ));
            }
            browser.apply_action(mez_mux::record_browser::RecordBrowserAction::OpenActive)?;
        }
        let page = browser.render_page();
        self.register_pending_record_browser_overlay(
            pane_id,
            "show-approvals",
            browser,
            Some(RuntimeRecordBrowserOverlaySource::Approvals),
        );
        Ok(AgentShellCommandOutcome::Display {
            command: "show-approvals".to_string(),
            body: page.raw_markdown,
        })
    }

    /// Builds a deterministic session-wide browser for pending approvals.
    pub(crate) fn approval_record_browser(&self) -> Result<RecordBrowser> {
        let mut approvals = self.blocked_approvals().pending();
        approvals.sort_by(|left, right| {
            left.created_at_unix_seconds
                .cmp(&right.created_at_unix_seconds)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut browser = RecordBrowser::new(
            "Pending approvals",
            approvals.into_iter().map(approval_browser_record).collect(),
            Vec::new(),
        )?;
        browser.set_table_columns(vec![
            "Pane".to_string(),
            "Agent".to_string(),
            "Action".to_string(),
            "Summary".to_string(),
        ]);
        browser.set_help(
            Some("**Keys:** `Enter` open · `a` approve once · `d` deny · `/` search".to_string()),
            Some("**Keys:** `Esc` back · `a` approve once · `d` deny · `/` search".to_string()),
        );
        browser.set_empty_message(Some("No pending approvals.".to_string()));
        Ok(browser)
    }

    /// Executes `/show-context` for the active pane conversation.
    pub(super) fn execute_agent_shell_show_context_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("show-context command must be a slash command")
        })?;
        let args = slash.args.split_whitespace().collect::<Vec<_>>();
        if args.len() > 1 {
            return Err(MezError::invalid_args(
                "show-context accepts at most one transcript sequence",
            ));
        }
        let detail_sequence = args
            .first()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    MezError::invalid_args("show-context transcript sequence must be an integer")
                })
            })
            .transpose()?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("show-context requires an active pane session")
            })?;
        let mut browser = self.context_record_browser(&conversation_id, pane_id)?;
        if let Some(sequence) = detail_sequence {
            if !browser.set_active_record_id(&sequence.to_string()) {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "context entry was not found in the active pane",
                ));
            }
            browser.apply_action(mez_mux::record_browser::RecordBrowserAction::OpenActive)?;
        }
        let source = Some(RuntimeRecordBrowserOverlaySource::Context {
            conversation_id,
            pane_id: pane_id.to_string(),
        });
        let page = browser.render_page();
        self.register_pending_record_browser_overlay(pane_id, "show-context", browser, source);
        Ok(AgentShellCommandOutcome::Display {
            command: "show-context".to_string(),
            body: page.raw_markdown,
        })
    }

    /// Loads one pane conversation into the shared record browser.
    fn context_record_browser(
        &self,
        conversation_id: &str,
        pane_id: &str,
    ) -> Result<RecordBrowser> {
        let store = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(|| MezError::invalid_state("show-context requires transcript storage"))?;
        let entries = match store.inspect(conversation_id) {
            Ok(entries) => entries,
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut browser = RecordBrowser::new(
            "Context",
            entries
                .into_iter()
                .filter(|entry| entry.pane_id == pane_id)
                .map(context_browser_record)
                .collect(),
            Vec::new(),
        )?;
        browser.enable_deletion();
        Ok(browser)
    }

    /// Deletes one selected record through its retained backend source and
    /// returns a refreshed browser with a valid active selection.
    pub(crate) fn delete_record_browser_entry(
        &mut self,
        source: &RuntimeRecordBrowserOverlaySource,
        record_id: &str,
        active_index: usize,
    ) -> Result<RecordBrowser> {
        let mut browser = match source {
            RuntimeRecordBrowserOverlaySource::Approvals => {
                return Err(MezError::invalid_state(
                    "approval browser records cannot be deleted",
                ));
            }
            RuntimeRecordBrowserOverlaySource::Context {
                conversation_id,
                pane_id,
            } => self.delete_context_browser_record(conversation_id, pane_id, record_id)?,
            RuntimeRecordBrowserOverlaySource::Issues {
                project_glob,
                kind,
                state,
                text,
                limit,
                ..
            } => {
                let config_root = self
                    .integration
                    .config_root()
                    .map(|path| path.to_path_buf())
                    .ok_or_else(|| {
                        MezError::config("show-issues requires a configured config root")
                    })?;
                let store = crate::storage::issues::IssueStore::from_database_path(
                    issues::runtime_issue_database_path(self, &config_root),
                );
                let query = mez_agent::issues::IssueBrowserQuery::new(
                    project_glob.clone(),
                    *kind,
                    *state,
                    text.clone(),
                    Some(*limit),
                )?;
                let record = store
                    .query_issue_browser(&query)?
                    .into_iter()
                    .find(|record| record.id == record_id)
                    .ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "issue browser record was not found",
                        )
                    })?;
                if !store.delete_issue(record.project, record.id)?.deleted {
                    return Err(MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "issue browser record was already deleted",
                    ));
                }
                self.refresh_record_browser_overlay_source(source)?
            }
            RuntimeRecordBrowserOverlaySource::Memories { .. } => {
                let config_root = self.integration.config_root().ok_or_else(|| {
                    MezError::invalid_state(
                        "show-memories requires a configured Mezzanine config root",
                    )
                })?;
                let store =
                    crate::storage::memory::PersistentMemoryStore::under_config_root(config_root);
                if !store.delete(record_id)? {
                    return Err(MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "memory browser record was already deleted",
                    ));
                }
                self.refresh_record_browser_overlay_source(source)?
            }
        };
        browser.set_active_index(active_index);
        Ok(browser)
    }

    /// Deletes one pane-owned transcript record and refreshes its context browser.
    fn delete_context_browser_record(
        &mut self,
        conversation_id: &str,
        pane_id: &str,
        record_id: &str,
    ) -> Result<RecordBrowser> {
        let sequence = record_id.parse::<u64>().map_err(|_| {
            MezError::invalid_args("context browser record id must be a transcript sequence")
        })?;
        let store = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(|| MezError::invalid_state("show-context requires transcript storage"))?;
        let entries = store.inspect(conversation_id)?;
        let entry_index = entries
            .iter()
            .position(|entry| entry.sequence == sequence && entry.pane_id == pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "context entry was not found in the active pane",
                )
            })?;
        let transcript_entries = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.transcript_entries)
            .unwrap_or(0);
        let retained_start = entries
            .len()
            .saturating_sub(usize::try_from(transcript_entries).unwrap_or(usize::MAX));
        if !store.delete_entry(conversation_id, sequence)? {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "context entry was already deleted",
            ));
        }
        if entry_index >= retained_start {
            self.agent_shell_store_mut()
                .retain_recent_transcript_entries(pane_id, transcript_entries.saturating_sub(1))?;
        }
        self.context_record_browser(conversation_id, pane_id)
    }

    /// Executes `/show-issues` by querying issue records and rendering browser Markdown.
    pub(super) fn execute_agent_shell_show_issues_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("show-issues command must be a slash command"))?;
        if !issues::runtime_issues_enabled(self) {
            return Err(MezError::invalid_args(
                "show-issues requires issues.enabled to be true",
            ));
        }
        let Some(config_root) = self
            .integration
            .config_root()
            .map(|path| path.to_path_buf())
        else {
            return Err(MezError::config(
                "show-issues requires a configured config root",
            ));
        };
        let args = parse_show_issues_args(&slash.args)?;
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .unwrap_or_else(|| config_root.clone());
        let current_project =
            crate::storage::issues::project_key_for_working_directory(working_directory);
        let project_glob = args
            .project_glob
            .clone()
            .or_else(|| (!args.all_projects).then_some(current_project.clone()));
        let store = crate::storage::issues::IssueStore::from_database_path(
            issues::runtime_issue_database_path(self, &config_root),
        );
        let issue_state = if args.detail_id.is_some() {
            args.state
        } else {
            args.state.or(Some(mez_agent::issues::IssueState::Open))
        };
        let source = Some(RuntimeRecordBrowserOverlaySource::Issues {
            project_glob: project_glob.clone(),
            default_project_glob: project_glob.clone(),
            kind: args.kind,
            state: issue_state,
            text: args.text.clone(),
            limit: args.limit,
        });
        let records = if let Some(id) = args.detail_id.as_ref() {
            store
                .get_issue(current_project, id.clone())?
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            let query = mez_agent::issues::IssueBrowserQuery::new(
                project_glob.clone(),
                args.kind,
                issue_state,
                args.text.clone(),
                Some(args.limit),
            )?;
            store.query_issue_browser(&query)?
        };
        let mut browser = RecordBrowser::new(
            if args.detail_id.is_some() {
                "Issue detail"
            } else {
                "Issues"
            },
            records.into_iter().map(issue_browser_record).collect(),
            issue_kind_filter_choices(),
        )?;
        browser.enable_deletion();
        if let Some(source) = source.as_ref() {
            set_record_browser_scope_indicator(&mut browser, source);
        }
        if args.detail_id.is_some() {
            browser.show_first_record_detail();
        }
        let page = browser.render_page();
        if let Some(path) = args.save_path {
            return self.save_record_browser_page(pane_id, "show-issues", path, page.raw_markdown);
        }
        self.register_pending_record_browser_overlay(pane_id, "show-issues", browser, source);
        Ok(AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        })
    }

    /// Executes `/show-memories` by querying persistent memory and rendering browser Markdown.
    pub(super) fn execute_agent_shell_show_memories_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("show-memories command must be a slash command")
        })?;
        if !self.runtime_persistent_memory_enabled() {
            return Err(MezError::invalid_state(
                "show-memories requires persistent memory to be enabled; run /memory on first",
            ));
        }
        let Some(config_root) = self
            .integration
            .config_root()
            .map(|path| path.to_path_buf())
        else {
            return Err(MezError::invalid_state(
                "show-memories requires a configured Mezzanine config root",
            ));
        };
        let args = parse_show_memories_args(&slash.args)?;
        let store = crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root);
        let scope = if args.all_scopes {
            None
        } else {
            Some(
                args.scope
                    .unwrap_or_else(|| self.runtime_remember_scope_for_pane(pane_id)),
            )
        };
        let memory_state = Some(args.state.unwrap_or(mez_agent::memory::MemoryState::Active));
        let source = Some(RuntimeRecordBrowserOverlaySource::Memories {
            scope: scope.clone(),
            default_scope: scope.clone(),
            kind: args.kind,
            state: memory_state,
            text: args.text.clone(),
            limit: args.limit,
        });
        let records = if let Some(id) = args.detail_id.as_ref() {
            vec![store.inspect(id)?]
        } else {
            store
                .search(&MemorySearchRequest {
                    query: args.text.clone(),
                    scope: scope.clone(),
                    kind: args.kind,
                    state: memory_state,
                    source: None,
                    limit: args.limit,
                })?
                .into_iter()
                .map(|result| result.record)
                .collect()
        };
        let mut browser = RecordBrowser::new(
            if args.detail_id.is_some() {
                "Memory detail"
            } else {
                "Memories"
            },
            records.into_iter().map(memory_browser_record).collect(),
            memory_kind_filter_choices(),
        )?;
        browser.enable_deletion();
        if let Some(source) = source.as_ref() {
            set_record_browser_scope_indicator(&mut browser, source);
        }
        if args.detail_id.is_some() {
            browser.show_first_record_detail();
        }
        let page = browser.render_page();
        if let Some(path) = args.save_path {
            return self.save_record_browser_page(
                pane_id,
                "show-memories",
                path,
                page.raw_markdown,
            );
        }
        self.register_pending_record_browser_overlay(pane_id, "show-memories", browser, source);
        Ok(AgentShellCommandOutcome::Display {
            command: "show-memories".to_string(),
            body: page.raw_markdown,
        })
    }

    fn save_record_browser_page(
        &self,
        pane_id: &str,
        command: &str,
        path: String,
        markdown: String,
    ) -> Result<AgentShellCommandOutcome> {
        let destination = record_browser_save_destination(self, pane_id, &path);
        fs::write(&destination, markdown)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: command.to_string(),
            body: format!(
                "{} saved path={}",
                command,
                json_escape(&destination.to_string_lossy())
            ),
            visibility,
        })
    }

    /// Refreshes a retained record-browser overlay from its backend query context.
    pub(crate) fn refresh_record_browser_overlay_source(
        &self,
        source: &RuntimeRecordBrowserOverlaySource,
    ) -> Result<RecordBrowser> {
        match source {
            RuntimeRecordBrowserOverlaySource::Approvals => self.approval_record_browser(),
            RuntimeRecordBrowserOverlaySource::Context {
                conversation_id,
                pane_id,
            } => self.context_record_browser(conversation_id, pane_id),
            RuntimeRecordBrowserOverlaySource::Issues {
                project_glob,
                kind,
                state,
                text,
                limit,
                ..
            } => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(|path| path.to_path_buf())
                else {
                    return Err(MezError::config(
                        "show-issues requires a configured config root",
                    ));
                };
                let store = crate::storage::issues::IssueStore::from_database_path(
                    issues::runtime_issue_database_path(self, &config_root),
                );
                let query = mez_agent::issues::IssueBrowserQuery::new(
                    project_glob.clone(),
                    *kind,
                    *state,
                    text.clone(),
                    Some(*limit),
                )?;
                let mut browser = RecordBrowser::new(
                    "Issues",
                    store
                        .query_issue_browser(&query)?
                        .into_iter()
                        .map(issue_browser_record)
                        .collect(),
                    issue_kind_filter_choices(),
                )?;
                browser.enable_deletion();
                set_record_browser_scope_indicator(&mut browser, source);
                Ok(browser)
            }
            RuntimeRecordBrowserOverlaySource::Memories {
                scope,
                kind,
                state,
                text,
                limit,
                ..
            } => {
                let Some(config_root) = self
                    .integration
                    .config_root()
                    .map(|path| path.to_path_buf())
                else {
                    return Err(MezError::invalid_state(
                        "show-memories requires a configured Mezzanine config root",
                    ));
                };
                let store =
                    crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root);
                let mut browser = RecordBrowser::new(
                    "Memories",
                    store
                        .search(&MemorySearchRequest {
                            query: text.clone(),
                            scope: scope.clone(),
                            kind: *kind,
                            state: *state,
                            source: None,
                            limit: *limit,
                        })?
                        .into_iter()
                        .map(|result| memory_browser_record(result.record))
                        .collect(),
                    memory_kind_filter_choices(),
                )?;
                browser.enable_deletion();
                set_record_browser_scope_indicator(&mut browser, source);
                Ok(browser)
            }
        }
    }

    /// Toggles a retained record browser between its default and global scope.
    pub(crate) fn record_browser_source_toggled_scope(
        &self,
        source: &RuntimeRecordBrowserOverlaySource,
    ) -> RuntimeRecordBrowserOverlaySource {
        match source {
            RuntimeRecordBrowserOverlaySource::Approvals => source.clone(),
            RuntimeRecordBrowserOverlaySource::Context { .. } => source.clone(),
            RuntimeRecordBrowserOverlaySource::Issues {
                project_glob,
                default_project_glob,
                kind,
                state,
                text,
                limit,
            } => RuntimeRecordBrowserOverlaySource::Issues {
                project_glob: if project_glob.is_some() {
                    None
                } else {
                    default_project_glob.clone()
                },
                default_project_glob: default_project_glob.clone(),
                kind: *kind,
                state: *state,
                text: text.clone(),
                limit: *limit,
            },
            RuntimeRecordBrowserOverlaySource::Memories {
                scope,
                default_scope,
                kind,
                state,
                text,
                limit,
            } => RuntimeRecordBrowserOverlaySource::Memories {
                scope: if scope.is_some() {
                    None
                } else {
                    default_scope.clone()
                },
                default_scope: default_scope.clone(),
                kind: *kind,
                state: *state,
                text: text.clone(),
                limit: *limit,
            },
        }
    }

    /// Returns a retained browser source updated for one submitted modal filter.
    pub(crate) fn record_browser_source_with_filter(
        &self,
        source: &RuntimeRecordBrowserOverlaySource,
        field: RecordBrowserFilterField,
        value: &str,
    ) -> Result<RuntimeRecordBrowserOverlaySource> {
        let value = value.trim();
        match source {
            RuntimeRecordBrowserOverlaySource::Approvals => Ok(source.clone()),
            RuntimeRecordBrowserOverlaySource::Context { .. } => Ok(source.clone()),
            RuntimeRecordBrowserOverlaySource::Issues {
                project_glob,
                default_project_glob,
                kind,
                state,
                text,
                limit,
            } => Ok(RuntimeRecordBrowserOverlaySource::Issues {
                project_glob: if field == RecordBrowserFilterField::ProjectGlob {
                    (!value.is_empty()).then(|| value.to_string())
                } else {
                    project_glob.clone()
                },
                default_project_glob: default_project_glob.clone(),
                kind: if field == RecordBrowserFilterField::Kind {
                    (!value.is_empty())
                        .then(|| mez_agent::issues::IssueKind::parse(value))
                        .transpose()?
                } else {
                    *kind
                },
                state: *state,
                text: if field == RecordBrowserFilterField::Text {
                    (!value.is_empty()).then(|| value.to_string())
                } else {
                    text.clone()
                },
                limit: *limit,
            }),
            RuntimeRecordBrowserOverlaySource::Memories {
                scope,
                default_scope,
                kind,
                state,
                text,
                limit,
            } => Ok(RuntimeRecordBrowserOverlaySource::Memories {
                scope: if field == RecordBrowserFilterField::ProjectGlob {
                    if value.is_empty() {
                        None
                    } else if value == "global" {
                        Some(mez_agent::memory::MemoryScope::Global)
                    } else {
                        Some(mez_agent::memory::MemoryScope::Project {
                            root: value.to_string(),
                        })
                    }
                } else {
                    scope.clone()
                },
                default_scope: default_scope.clone(),
                kind: if field == RecordBrowserFilterField::Kind {
                    (!value.is_empty())
                        .then(|| parse_kind(value).map_err(MezError::from))
                        .transpose()?
                } else {
                    *kind
                },
                state: *state,
                text: if field == RecordBrowserFilterField::Text {
                    (!value.is_empty()).then(|| value.to_string())
                } else {
                    text.clone()
                },
                limit: *limit,
            }),
        }
    }

    /// Writes retained record-browser Markdown using pane-relative path rules.
    pub(crate) fn save_record_browser_overlay_markdown(
        &self,
        pane_id: &str,
        path: &str,
        markdown: String,
    ) -> Result<PathBuf> {
        let destination = record_browser_save_destination(self, pane_id, path);
        fs::write(&destination, markdown)?;
        Ok(destination)
    }
}

/// Applies the retained backend scope as visible record-browser context.
fn set_record_browser_scope_indicator(
    browser: &mut RecordBrowser,
    source: &RuntimeRecordBrowserOverlaySource,
) {
    let indicator = match source {
        RuntimeRecordBrowserOverlaySource::Approvals => "live session".to_string(),
        RuntimeRecordBrowserOverlaySource::Context { pane_id, .. } => {
            format!("current pane {pane_id}")
        }
        RuntimeRecordBrowserOverlaySource::Issues { project_glob, .. } => project_glob
            .clone()
            .unwrap_or_else(|| "all projects".to_string()),
        RuntimeRecordBrowserOverlaySource::Memories { scope, .. } => scope
            .as_ref()
            .map(runtime_remember_scope_display)
            .unwrap_or_else(|| "all scopes".to_string()),
    };
    browser.set_scope_indicator(Some(indicator));
}

/// Projects one pending approval into a single-link browser record.
fn approval_browser_record(
    approval: &mez_agent::permissions::BlockedApprovalRequest,
) -> RecordBrowserRecord {
    let metadata = vec![
        ("Pane".to_string(), approval.pane_id.clone()),
        ("Agent".to_string(), approval.requesting_agent_id.clone()),
        ("Action".to_string(), approval.action_kind.clone()),
        ("Summary".to_string(), approval.action_summary.clone()),
        (
            "Created".to_string(),
            approval
                .created_at_unix_seconds
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ),
    ];
    let mut markdown = vec![format!("**Summary:** {}", approval.action_summary)];
    if !approval.declared_effects.is_empty() {
        markdown.push(format!(
            "**Declared effects:** {}",
            approval.declared_effects.join(", ")
        ));
    }
    if !approval.matched_rules.is_empty() {
        markdown.push(format!(
            "**Matched rules:** {}",
            approval.matched_rules.join(", ")
        ));
    }
    if !approval.parent_agent_chain.is_empty() {
        markdown.push(format!(
            "**Parent agent chain:** {}",
            approval.parent_agent_chain.join(" → ")
        ));
    }
    RecordBrowserRecord {
        id: approval.id.clone(),
        open_command: Some(format!("/show-approvals {}", approval.id)),
        title: format!("{} — {}", approval.action_kind, approval.action_summary),
        metadata,
        markdown: markdown.join("\n\n"),
    }
}

#[derive(Debug, Default)]
struct ShowIssuesArgs {
    project_glob: Option<String>,
    all_projects: bool,
    kind: Option<mez_agent::issues::IssueKind>,
    state: Option<mez_agent::issues::IssueState>,
    text: Option<String>,
    limit: usize,
    save_path: Option<String>,
    detail_id: Option<String>,
}

#[derive(Debug, Default)]
struct ShowMemoriesArgs {
    all_scopes: bool,
    scope: Option<mez_agent::memory::MemoryScope>,
    kind: Option<mez_agent::memory::MemoryKind>,
    state: Option<mez_agent::memory::MemoryState>,
    text: Option<String>,
    limit: usize,
    save_path: Option<String>,
    detail_id: Option<String>,
}

fn parse_show_issues_args(args: &str) -> Result<ShowIssuesArgs> {
    let mut parsed = ShowIssuesArgs {
        limit: DEFAULT_SHOW_RECORD_LIMIT,
        ..ShowIssuesArgs::default()
    };
    let Some(tokens) = shlex::split(args) else {
        return Err(MezError::invalid_args(
            "show-issues arguments contain an unterminated quote",
        ));
    };
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--project" | "--project-glob" => {
                index = index.saturating_add(1);
                parsed.project_glob =
                    Some(required_show_value(&tokens, index, "project")?.to_string());
            }
            "--all-projects" => parsed.all_projects = true,
            "--kind" => {
                index = index.saturating_add(1);
                parsed.kind = Some(mez_agent::issues::IssueKind::parse(required_show_value(
                    &tokens, index, "kind",
                )?)?);
            }
            "--state" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "state")?;
                parsed.state = if value == "all" {
                    None
                } else {
                    Some(mez_agent::issues::IssueState::parse(value)?)
                };
            }
            "--text" | "--query" => {
                index = index.saturating_add(1);
                parsed.text = Some(required_show_value(&tokens, index, "text")?.to_string());
            }
            "--limit" => {
                index = index.saturating_add(1);
                parsed.limit = parse_show_limit(required_show_value(&tokens, index, "limit")?)?;
            }
            "--save" => {
                index = index.saturating_add(1);
                parsed.save_path = Some(required_show_value(&tokens, index, "save")?.to_string());
            }
            token if token.starts_with('-') => {
                return Err(MezError::invalid_args(
                    "show-issues accepts --project, --all-projects, --kind, --state, --text, --limit, --save, and an optional issue id",
                ));
            }
            id => {
                if parsed.detail_id.replace(id.to_string()).is_some() {
                    return Err(MezError::invalid_args(
                        "show-issues accepts at most one issue id",
                    ));
                }
            }
        }
        index = index.saturating_add(1);
    }
    Ok(parsed)
}

fn parse_show_memories_args(args: &str) -> Result<ShowMemoriesArgs> {
    let mut parsed = ShowMemoriesArgs {
        limit: DEFAULT_SHOW_RECORD_LIMIT,
        ..ShowMemoriesArgs::default()
    };
    let Some(tokens) = shlex::split(args) else {
        return Err(MezError::invalid_args(
            "show-memories arguments contain an unterminated quote",
        ));
    };
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--scope" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "scope")?;
                match value {
                    "all" => parsed.all_scopes = true,
                    "global" => parsed.scope = Some(mez_agent::memory::MemoryScope::Global),
                    "project" => parsed.scope = None,
                    _ => {
                        return Err(MezError::invalid_args(
                            "show-memories --scope accepts project, global, or all",
                        ));
                    }
                }
            }
            "--all-scopes" => parsed.all_scopes = true,
            "--kind" => {
                index = index.saturating_add(1);
                parsed.kind = Some(
                    parse_kind(required_show_value(&tokens, index, "kind")?)
                        .map_err(MezError::from)?,
                );
            }
            "--state" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "state")?;
                parsed.state = if value == "all" {
                    None
                } else {
                    Some(parse_state(value).map_err(MezError::from)?)
                };
            }
            "--text" | "--query" => {
                index = index.saturating_add(1);
                parsed.text = Some(required_show_value(&tokens, index, "text")?.to_string());
            }
            "--limit" => {
                index = index.saturating_add(1);
                parsed.limit = parse_show_limit(required_show_value(&tokens, index, "limit")?)?;
            }
            "--save" => {
                index = index.saturating_add(1);
                parsed.save_path = Some(required_show_value(&tokens, index, "save")?.to_string());
            }
            token if token.starts_with('-') => {
                return Err(MezError::invalid_args(
                    "show-memories accepts --scope, --all-scopes, --kind, --state, --text, --limit, --save, and an optional memory id",
                ));
            }
            id => {
                if parsed.detail_id.replace(id.to_string()).is_some() {
                    return Err(MezError::invalid_args(
                        "show-memories accepts at most one memory id",
                    ));
                }
            }
        }
        index = index.saturating_add(1);
    }
    Ok(parsed)
}

fn required_show_value<'a>(tokens: &'a [String], index: usize, name: &str) -> Result<&'a str> {
    let value = tokens
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("show option --{name} requires a value")))?;
    if value.starts_with("--") {
        return Err(MezError::invalid_args(format!(
            "show option --{name} requires a value"
        )));
    }
    Ok(value)
}

fn parse_show_limit(value: &str) -> Result<usize> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("show command --limit must be an integer"))?;
    if limit == 0 {
        return Err(MezError::invalid_args(
            "show command --limit must be positive",
        ));
    }
    Ok(limit.min(DEFAULT_SHOW_RECORD_LIMIT))
}

fn context_browser_record(entry: mez_agent::transcript::TranscriptEntry) -> RecordBrowserRecord {
    let role = match entry.role {
        mez_agent::transcript::TranscriptRole::User => "user",
        mez_agent::transcript::TranscriptRole::Assistant => "assistant",
        mez_agent::transcript::TranscriptRole::Tool => "tool",
        mez_agent::transcript::TranscriptRole::System => "system",
    };
    let preview = entry
        .content
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.chars().take(80).collect::<String>())
        .unwrap_or_else(|| "empty entry".to_string());
    RecordBrowserRecord {
        id: entry.sequence.to_string(),
        open_command: Some(format!("/show-context {}", entry.sequence)),
        title: format!("{role} · {preview}"),
        metadata: vec![
            ("sequence".to_string(), entry.sequence.to_string()),
            ("role".to_string(), role.to_string()),
            ("turn_id".to_string(), entry.turn_id),
            ("agent_id".to_string(), entry.agent_id),
            ("pane_id".to_string(), entry.pane_id),
            (
                "created_at_unix_seconds".to_string(),
                entry.created_at_unix_seconds.to_string(),
            ),
        ],
        markdown: entry.content,
    }
}

fn issue_browser_record(record: mez_agent::issues::IssueRecord) -> RecordBrowserRecord {
    let markdown = issue_record_markdown(&record);
    RecordBrowserRecord {
        id: record.id.clone(),
        open_command: Some(format!("/show-issues {}", record.id)),
        title: record.title.clone(),
        metadata: vec![
            ("id".to_string(), record.id),
            ("project".to_string(), record.project),
            ("kind".to_string(), record.kind.as_str().to_string()),
            ("state".to_string(), record.state.as_str().to_string()),
            (
                "depends_on".to_string(),
                serde_json::json!(record.depends_on).to_string(),
            ),
            (
                "created_at_unix_seconds".to_string(),
                record.created_at_unix_seconds.to_string(),
            ),
            (
                "updated_at_unix_seconds".to_string(),
                record.updated_at_unix_seconds.to_string(),
            ),
        ],
        markdown,
    }
}

fn issue_record_markdown(record: &mez_agent::issues::IssueRecord) -> String {
    let mut lines = Vec::new();
    if let Some(body) = record.body.as_deref() {
        lines.push(body.to_string());
    } else {
        lines.push("_No issue body._".to_string());
    }
    if let Some(notes) = record.notes.as_deref() {
        lines.push(String::new());
        lines.push("## Notes".to_string());
        lines.push(notes.to_string());
    }
    lines.join("\n")
}

fn memory_browser_record(record: mez_agent::memory::MemoryRecord) -> RecordBrowserRecord {
    RecordBrowserRecord {
        id: record.id.clone(),
        open_command: Some(format!("/show-memories {}", record.id)),
        title: memory_record_title(&record),
        metadata: vec![
            ("id".to_string(), record.id),
            (
                "scope".to_string(),
                runtime_remember_scope_display(&record.scope),
            ),
            ("kind".to_string(), kind_name(record.kind).to_string()),
            ("state".to_string(), state_name(record.state).to_string()),
            ("source".to_string(), source_name(record.source).to_string()),
            ("priority".to_string(), record.priority.to_string()),
            (
                "created_at_unix_seconds".to_string(),
                record.created_at_unix_seconds.to_string(),
            ),
            (
                "updated_at_unix_seconds".to_string(),
                record.updated_at_unix_seconds.to_string(),
            ),
        ],
        markdown: record.content,
    }
}

fn memory_record_title(record: &mez_agent::memory::MemoryRecord) -> String {
    record
        .content
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_else(|| record.id.clone())
}

fn issue_kind_filter_choices() -> Vec<RecordBrowserFilterChoice> {
    vec![
        RecordBrowserFilterChoice {
            label: "all kinds".to_string(),
            value: String::new(),
        },
        RecordBrowserFilterChoice {
            label: mez_agent::issues::IssueKind::Defect.as_str().to_string(),
            value: mez_agent::issues::IssueKind::Defect.as_str().to_string(),
        },
        RecordBrowserFilterChoice {
            label: mez_agent::issues::IssueKind::Task.as_str().to_string(),
            value: mez_agent::issues::IssueKind::Task.as_str().to_string(),
        },
    ]
}

fn memory_kind_filter_choices() -> Vec<RecordBrowserFilterChoice> {
    vec![
        RecordBrowserFilterChoice {
            label: "all kinds".to_string(),
            value: String::new(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Preference).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Preference).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Fact).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Fact).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Procedure).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Procedure).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Documentation).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Documentation).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Research).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Research).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Episode).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Episode).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Warning).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Warning).to_string(),
        },
        RecordBrowserFilterChoice {
            label: kind_name(mez_agent::memory::MemoryKind::Scratch).to_string(),
            value: kind_name(mez_agent::memory::MemoryKind::Scratch).to_string(),
        },
    ]
}

fn record_browser_save_destination(
    service: &RuntimeSessionService,
    pane_id: &str,
    input: &str,
) -> PathBuf {
    let path = PathBuf::from(input);
    if path.is_absolute() {
        return path;
    }
    service
        .pane_current_working_directory(pane_id)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(path)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SHOW_RECORD_LIMIT, parse_show_issues_args, parse_show_memories_args};

    /// Verifies `/show-issues` accepts the browser filters and save path used
    /// by the interactive issue browser command surface.
    #[test]
    fn show_issues_parser_accepts_filters_detail_and_save_path() {
        let parsed = parse_show_issues_args(
            "--project /repo/* --kind task --state resolved --text panic --limit 20 --save out.md issue-1",
        )
        .unwrap();

        assert_eq!(parsed.project_glob.as_deref(), Some("/repo/*"));
        assert_eq!(parsed.kind, Some(mez_agent::issues::IssueKind::Task));
        assert_eq!(parsed.state, Some(mez_agent::issues::IssueState::Resolved));
        assert_eq!(parsed.text.as_deref(), Some("panic"));
        assert_eq!(parsed.limit, 20);
        assert_eq!(parsed.save_path.as_deref(), Some("out.md"));
        assert_eq!(parsed.detail_id.as_deref(), Some("issue-1"));
    }

    /// Verifies `/show-memories` defaults remain bounded and parse the shared
    /// memory filters used by the record-browser adapter.
    #[test]
    fn show_memories_parser_accepts_scope_kind_state_text_and_limit() {
        let parsed = parse_show_memories_args(
            "--scope all --kind documentation --state stale --text maap --limit 250 memory-1",
        )
        .unwrap();

        assert!(parsed.all_scopes);
        assert_eq!(
            parsed.kind,
            Some(mez_agent::memory::MemoryKind::Documentation)
        );
        assert_eq!(parsed.state, Some(mez_agent::memory::MemoryState::Stale));
        assert_eq!(parsed.text.as_deref(), Some("maap"));
        assert_eq!(parsed.limit, DEFAULT_SHOW_RECORD_LIMIT);
        assert_eq!(parsed.detail_id.as_deref(), Some("memory-1"));
    }
}
