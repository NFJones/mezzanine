//! Agent-shell saved-session resume command helpers.
//!
//! This module keeps `/resume` transcript replay logic out of the broader
//! command dispatcher. It owns saved-session browsing, directory restoration,
//! transcript display fallback formatting, and durable session naming.

use super::{
    AgentShellCommandOutcome, MezError, PathBuf, Result, RuntimeSessionService, SplitDirection,
    TranscriptEntry, TranscriptRole, current_unix_seconds, json_escape, parse_slash_command,
    runtime_fit_status_line, runtime_markdown_table, session_state_name, shell_command_from_argv,
    unix_seconds_to_rfc3339,
};
use base64::Engine;
use mez_agent::transcript::ConversationSummary;
use mez_mux::readline::ReadlineEdit;
use mez_mux::record_browser::{RecordBrowser, RecordBrowserRecord};

use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::storage::transcript::SavedAgentSession;

/// Maximum saved transcript entries to render when `/resume` has no presentation log.
const AGENT_RESUME_TRANSCRIPT_REPLAY_ENTRIES: usize = 64;
/// Maximum transcript bytes to read for `/resume` fallback replay.
const AGENT_RESUME_TRANSCRIPT_REPLAY_BYTES: u64 = 2 * 1024 * 1024;

/// Returns the saved working directory from transcript context entries.
///
/// # Parameters
/// - `entries`: The durable transcript entries for one conversation.
fn runtime_resume_directory_from_entries(entries: &[TranscriptEntry]) -> Option<String> {
    let mut project_root = None;
    for entry in entries {
        for line in entry.content.lines() {
            if let Some(value) = line
                .strip_prefix("cwd=")
                .or_else(|| line.strip_prefix("working_directory="))
                && !value.trim().is_empty()
            {
                return Some(value.trim().to_string());
            }
            if project_root.is_none()
                && let Some(value) = line.strip_prefix("project_root=")
                && !value.trim().is_empty()
            {
                project_root = Some(value.trim().to_string());
            }
        }
    }
    project_root
}

/// Returns the saved working directory from bounded conversation metadata.
fn runtime_resume_directory_from_summary(summary: &ConversationSummary) -> Option<String> {
    summary
        .directory
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Formats saved system transcript metadata for human resume replay.
///
/// # Parameters
/// - `content`: The saved system transcript entry body.
fn runtime_resume_system_display_content(content: &str) -> String {
    let entry = TranscriptEntry {
        conversation_id: "resume-display".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "resume-display".to_string(),
        agent_id: "agent-resume-display".to_string(),
        pane_id: "%resume-display".to_string(),
        content: content.to_string(),
    };
    runtime_resume_directory_from_entries(&[entry])
        .map(|directory| format!("Session directory: {directory}"))
        .unwrap_or_else(|| content.to_string())
}

/// Escapes user-assigned names without extending the UUID command link.
fn escape_session_name_for_markdown(name: &str) -> String {
    name.chars()
        .flat_map(|character| {
            if matches!(character, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>') {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect()
}

impl RuntimeSessionService {
    /// Assigns or replaces the durable display name for the current conversation.
    pub(super) fn execute_agent_shell_name_session_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("name-session command must be a slash command")
        })?;
        let name = invocation.args.trim();
        if name.is_empty() {
            return Err(MezError::invalid_args(
                "usage: /name-session <name>|--clear",
            ));
        }
        let clear_requested = name
            .split_whitespace()
            .any(|argument| argument == "--clear");
        if clear_requested && name != "--clear" {
            return Err(MezError::invalid_args(
                "usage: /name-session <name>|--clear",
            ));
        }
        let session = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        if session.ephemeral {
            return Err(MezError::invalid_state(
                "ephemeral agent sessions cannot be named",
            ));
        }
        let conversation_id = session.session_id.clone();
        let visibility = session.visibility;
        let directory = self
            .pane_current_working_directory(pane_id)
            .map(|path| path.to_string_lossy().into_owned());
        let store = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(|| MezError::invalid_state("transcript persistence is unavailable"))?;
        if name == "--clear" {
            let cleared = store.clear_session_name(&conversation_id)?;
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "name-session".to_string(),
                body: format!(
                    "conversation_id={} named=false cleared={cleared}",
                    conversation_id
                ),
                visibility,
            });
        }
        let named =
            store.name_session(&conversation_id, name, current_unix_seconds(), directory)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "name-session".to_string(),
            body: format!(
                "conversation_id={} name={} named=true",
                conversation_id,
                json_escape(&named.name)
            ),
            visibility,
        })
    }

    /// Runs the execute agent shell resume command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_resume_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("resume command must be a slash command"))?;
        let conversation_arg = invocation.args.split_whitespace().next();
        if conversation_arg.is_none() {
            return self.execute_agent_shell_resume_picker_command(pane_id);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "resume".to_string(),
                body: "conversations=0 source=unavailable".to_string(),
            });
        };
        let conversation_id = match conversation_arg {
            Some("--latest" | "latest") => {
                let sessions = store.saved_sessions()?;
                let Some(conversation_id) = Self::runtime_latest_agent_saved_session_id(&sessions)
                else {
                    return Ok(AgentShellCommandOutcome::Display {
                        command: "resume".to_string(),
                        body: "conversations=0 source=runtime-resume latest=false reason=no-saved-sessions"
                            .to_string(),
                    });
                };
                conversation_id
            }
            Some(conversation_id) => conversation_id.to_string(),
            None => unreachable!("bare resume returns through the picker before store lookup"),
        };
        let saved = store
            .saved_sessions()?
            .into_iter()
            .find(|session| session.summary.conversation_id == conversation_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "conversation transcript not found",
                )
            })?;
        let summary = saved.summary;
        let entries = if summary.entries == 0 {
            Vec::new()
        } else {
            store.inspect_recent(
                &conversation_id,
                AGENT_RESUME_TRANSCRIPT_REPLAY_ENTRIES,
                AGENT_RESUME_TRANSCRIPT_REPLAY_BYTES,
            )?
        };
        let presentation_entries = store.inspect_presentation(&conversation_id)?;
        let resume_directory = runtime_resume_directory_from_summary(&summary)
            .or_else(|| runtime_resume_directory_from_entries(&entries));
        let prepared_resume_state =
            self.prepare_agent_resume_state_for_conversation(&conversation_id)?;
        let previous_session = self
            .agent_shell_store()
            .get(pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        let previous_agent_screen = self
            .agent_pane_screen_state(pane_id)
            .map(|state| (state.conversation_id().to_string(), state.screen().clone()));
        let previous_presentation = self.snapshot_agent_resume_presentation(pane_id);
        let previous_transcript_refs = self.persistence.pane_transcript_refs(pane_id);
        let previous_working_directory = self.pane_current_working_directory(pane_id);

        let resume_result = (|| -> Result<(String, u64, mez_agent::AgentShellVisibility)> {
            let (session_id, transcript_entries, visibility) = {
                let session = self.agent_shell_store_mut().bind_conversation(
                    pane_id,
                    &conversation_id,
                    summary.entries as u64,
                )?;
                (
                    session.session_id.clone(),
                    session.transcript_entries,
                    session.visibility,
                )
            };
            self.reload_agent_prompt_history_for_pane(pane_id)?;
            if let Some(size) = self
                .agent_pane_screen(pane_id)
                .or_else(|| self.process_pane_screen(pane_id))
                .map(|screen| screen.size())
            {
                self.set_agent_pane_screen(
                    pane_id.to_string(),
                    session_id.clone(),
                    mez_terminal::TerminalScreen::new_with_history_config(
                        size,
                        self.terminal_history_limit(),
                        self.terminal_history_rotate_lines(),
                    )?,
                );
            }
            if !self.replay_agent_presentation_entries_to_terminal_buffer(
                pane_id,
                &presentation_entries,
            )? {
                self.set_agent_prompt_display_lines(
                    pane_id,
                    Self::runtime_resume_transcript_display(
                        &summary.conversation_id,
                        summary.entries,
                        &entries,
                    ),
                )?;
            }
            self.restore_agent_resume_directory(pane_id, resume_directory.as_deref())?;
            self.record_pane_transcript_ref(pane_id, format!("transcript:{pane_id}:{session_id}"))?;
            self.commit_prepared_agent_resume_state(pane_id, &session_id, prepared_resume_state)?;
            Ok((session_id, transcript_entries, visibility))
        })();
        let (session_id, transcript_entries, visibility) = match resume_result {
            Ok(result) => result,
            Err(error) => {
                self.agent_shell_store_mut()
                    .restore_session(pane_id, previous_session)?;
                if let Some((conversation_id, screen)) = previous_agent_screen {
                    self.set_agent_pane_screen(pane_id, conversation_id, screen);
                } else {
                    self.remove_agent_pane_screen(pane_id);
                }
                self.restore_agent_resume_presentation(pane_id, previous_presentation);
                self.persistence
                    .replace_pane_transcript_refs(pane_id, previous_transcript_refs);
                if let Some(path) = previous_working_directory {
                    self.set_pane_current_working_directory(pane_id, path);
                } else {
                    self.remove_pane_current_working_directory(pane_id);
                }
                return Err(error);
            }
        };
        Ok(AgentShellCommandOutcome::Mutated {
            command: "resume".to_string(),
            body: format!(
                "conversation_id={} entries={} pane={} resumed=true",
                session_id, transcript_entries, pane_id
            ),
            visibility,
        })
    }

    /// Returns the latest saved agent session using the same ordering as the
    /// saved-session picker.
    ///
    /// # Parameters
    /// - `summaries`: The saved conversation summaries to sort.
    fn runtime_latest_agent_saved_session_id(sessions: &[SavedAgentSession]) -> Option<String> {
        let mut sorted_sessions = sessions.iter().collect::<Vec<_>>();
        sorted_sessions.sort_by(|left, right| {
            right
                .summary
                .last_created_at_unix_seconds
                .cmp(&left.summary.last_created_at_unix_seconds)
                .then_with(|| {
                    right
                        .summary
                        .first_created_at_unix_seconds
                        .cmp(&left.summary.first_created_at_unix_seconds)
                })
                .then_with(|| {
                    left.summary
                        .conversation_id
                        .cmp(&right.summary.conversation_id)
                })
        });
        sorted_sessions
            .first()
            .map(|session| session.summary.conversation_id.clone())
    }

    /// Restores the pane to a saved session directory when that directory is
    /// still available.
    ///
    /// # Parameters
    /// - `pane_id`: The pane being rebound to the saved conversation.
    /// - `resume_directory`: The directory persisted with the saved session.
    fn restore_agent_resume_directory(
        &mut self,
        pane_id: &str,
        resume_directory: Option<&str>,
    ) -> Result<()> {
        let Some(resume_directory) = resume_directory.filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let path = PathBuf::from(resume_directory);
        if !path.is_dir() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "agent: resume directory unavailable; staying in current directory: {}",
                    runtime_fit_status_line(resume_directory, 160)
                ),
            )?;
            return Ok(());
        }
        self.set_pane_current_working_directory(pane_id, path.clone());
        if self.primary_pid_for_live_pane_process(pane_id).is_some() {
            let mut command =
                shell_command_from_argv(&["cd".to_string(), path.to_string_lossy().into_owned()])?;
            command.push('\n');
            if let Err(error) = self.write_runtime_pane_input(pane_id, command.as_bytes()) {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: resume directory recorded but shell cd failed: {}",
                        runtime_fit_status_line(error.message(), 160)
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// Opens the retained table browser used to select or delete saved sessions.
    fn execute_agent_shell_resume_picker_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let Some(_) = self.persistence.transcript_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "resume".to_string(),
                body: self.runtime_current_session_display(),
            });
        };
        let browser = self.saved_sessions_record_browser()?;
        let page = browser.render_page();
        self.register_pending_record_browser_overlay(
            pane_id,
            "resume",
            browser,
            Some(RuntimeRecordBrowserOverlaySource::SavedSessions),
        );
        Ok(AgentShellCommandOutcome::Display {
            command: "resume".to_string(),
            body: page.raw_markdown,
        })
    }

    /// Builds the shared table browser for durable saved agent conversations.
    pub(crate) fn saved_sessions_record_browser(&self) -> Result<RecordBrowser> {
        let store = self
            .persistence
            .transcript_store()
            .ok_or_else(|| MezError::invalid_state("resume requires transcript storage"))?;
        let mut sessions = store
            .saved_sessions()?
            .into_iter()
            .filter(|session| session.summary.latest_user_prompt.is_some())
            .collect::<Vec<_>>();
        Self::sort_agent_saved_sessions_for_picker(&mut sessions);
        let prompt_width = usize::from(self.session.authoritative_size.columns)
            .saturating_sub(40)
            .clamp(20, 80);
        let records = sessions
            .into_iter()
            .map(|session| Self::saved_session_browser_record(store, session, prompt_width))
            .collect::<Result<Vec<_>>>()?;
        let mut browser = RecordBrowser::new("Agent Sessions", records, Vec::new())?;
        browser.enable_deletion();
        browser.set_table_id_column("Conversation");
        browser.set_table_columns_with_labels(vec![
            ("Name".to_string(), "name".to_string()),
            ("Latest prompt".to_string(), "latest_prompt".to_string()),
            ("Last active".to_string(), "last_active".to_string()),
            ("Directory".to_string(), "directory".to_string()),
            ("Entries".to_string(), "entries".to_string()),
        ]);
        browser.set_help(
            Some(
                "**Keys:** `↑`/`↓` focus conversation UUID · `Enter` resume · `i` details · `c` clear name · `d` delete · `/` search"
                    .to_string(),
            ),
            Some("**Keys:** `Esc` back · `d` delete · `/` search".to_string()),
        );
        browser.set_empty_message(Some("No saved agent sessions are available.".to_string()));
        Ok(browser)
    }

    /// Formats the active in-memory session when transcript storage is unavailable.
    fn runtime_current_session_display(&self) -> String {
        let attached_clients = self
            .session
            .clients()
            .iter()
            .filter(|client| client.state == mez_mux::session::ClientState::Attached)
            .count();
        let last_attached_at = self
            .session
            .last_attached_at_unix_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "none".to_string());
        let mut lines = vec![
            "## Agent Sessions".to_string(),
            String::new(),
            "No saved agent transcript store is configured.".to_string(),
            String::new(),
            "### Live Mezzanine Session".to_string(),
            String::new(),
        ];
        let rows = vec![vec![
            self.session.id.to_string(),
            self.session.name.clone(),
            session_state_name(self.session.state).to_string(),
            unix_seconds_to_rfc3339(self.session.created_at_unix_seconds),
            last_attached_at,
            self.session.windows().len().to_string(),
            self.session.clients().len().to_string(),
            attached_clients.to_string(),
            self.session.primary_client_id().is_none().to_string(),
        ]];
        lines.extend(runtime_markdown_table(
            &[
                "Session",
                "Name",
                "State",
                "Created",
                "Last attached",
                "Windows",
                "Clients",
                "Attached clients",
                "Primary available",
            ],
            &rows,
        ));
        lines.join("\n")
    }

    /// Applies named-first ordering while retaining activity order per partition.
    fn sort_agent_saved_sessions_for_picker(sessions: &mut [SavedAgentSession]) {
        sessions.sort_by(|left, right| {
            right
                .name
                .is_some()
                .cmp(&left.name.is_some())
                .then_with(|| {
                    right
                        .summary
                        .last_created_at_unix_seconds
                        .cmp(&left.summary.last_created_at_unix_seconds)
                        .then_with(|| {
                            right
                                .summary
                                .first_created_at_unix_seconds
                                .cmp(&left.summary.first_created_at_unix_seconds)
                        })
                        .then_with(|| {
                            left.summary
                                .conversation_id
                                .cmp(&right.summary.conversation_id)
                        })
                })
        });
    }

    /// Adapts one saved conversation to the shared record-browser contract.
    fn saved_session_browser_record(
        store: &crate::storage::transcript::AgentTranscriptStore,
        session: SavedAgentSession,
        prompt_width: usize,
    ) -> Result<RecordBrowserRecord> {
        let summary = session.summary;
        let transcript_markdown = if summary.entries == 0 {
            "No saved transcript entries were found for this session.".to_string()
        } else {
            Self::saved_session_transcript_markdown(&store.inspect(&summary.conversation_id)?)
        };
        let escaped_name = session
            .name
            .as_deref()
            .map(escape_session_name_for_markdown);
        let title = escaped_name
            .as_deref()
            .map(|name| format!("{} - {name}", summary.conversation_id))
            .unwrap_or_else(|| summary.conversation_id.clone());
        Ok(RecordBrowserRecord {
            id: summary.conversation_id.clone(),
            open_command: Some(format!("/resume {}", summary.conversation_id)),
            title,
            metadata: vec![
                ("name".to_string(), session.name.unwrap_or_default()),
                (
                    "last_active".to_string(),
                    unix_seconds_to_rfc3339(summary.last_created_at_unix_seconds),
                ),
                (
                    "directory".to_string(),
                    summary.directory.unwrap_or_else(|| "-".to_string()),
                ),
                ("entries".to_string(), summary.entries.to_string()),
                (
                    "latest_prompt".to_string(),
                    runtime_fit_status_line(
                        summary.latest_user_prompt.as_deref().unwrap_or("-"),
                        prompt_width,
                    ),
                ),
            ],
            markdown: transcript_markdown,
        })
    }

    /// Formats every durable transcript entry for saved-session inspection.
    fn saved_session_transcript_markdown(entries: &[TranscriptEntry]) -> String {
        let mut sections = Vec::with_capacity(entries.len());
        for entry in entries {
            let role = match entry.role {
                TranscriptRole::User => "User",
                TranscriptRole::Assistant => "Assistant",
                TranscriptRole::Tool => "Tool",
                TranscriptRole::System => "System",
            };
            let content = Self::runtime_resume_entry_display_content(entry);
            let body = if content.is_empty() {
                "    (empty)".to_string()
            } else {
                content
                    .lines()
                    .map(|line| format!("    {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            sections.push(format!("## {role} entry {}\n\n{body}", entry.sequence));
        }
        sections.join("\n\n")
    }

    /// Formats a resumed transcript as prompt display lines so the user can
    /// pick up the saved conversation with visible context in the pane.
    pub(crate) fn runtime_resume_transcript_display(
        conversation_id: &str,
        transcript_entries: usize,
        entries: &[TranscriptEntry],
    ) -> Vec<String> {
        let mut lines = vec!["Resumed Agent Session".to_string()];
        if entries.is_empty() {
            lines.push("No saved transcript entries were found.".to_string());
            return lines;
        }
        lines.push(format!(
            "Conversation ID: {} | Entries: {} | Resumed: yes",
            json_escape(conversation_id),
            transcript_entries
        ));
        lines.push(String::new());
        for entry in entries {
            let content = Self::runtime_resume_entry_display_content(entry);
            if content.trim().is_empty() {
                continue;
            }
            let prefix = match entry.role {
                TranscriptRole::User => "user> ",
                TranscriptRole::Assistant => "mez> ",
                TranscriptRole::Tool => "agent: ",
                TranscriptRole::System => "system> ",
            };
            lines.push(format!(
                "{}{}",
                prefix,
                Self::runtime_resume_entry_preview(&content)
            ));
        }
        lines
    }

    /// Builds user-visible content for one resumed transcript entry.
    fn runtime_resume_entry_display_content(entry: &TranscriptEntry) -> String {
        match entry.role {
            TranscriptRole::Tool => Self::runtime_resume_tool_display_content(&entry.content),
            TranscriptRole::System => runtime_resume_system_display_content(&entry.content),
            TranscriptRole::User | TranscriptRole::Assistant => {
                Self::runtime_resume_best_effort_text(&entry.content)
            }
        }
    }

    /// Extracts the human-facing text from stored tool transcript content.
    fn runtime_resume_tool_display_content(content: &str) -> String {
        let text = Self::runtime_resume_best_effort_text(content);
        if let Some(extracted) = Self::runtime_resume_structured_text(&text) {
            return extracted;
        }
        if let Some(extracted) = Self::runtime_resume_content_field_text(&text) {
            return extracted;
        }
        text
    }

    /// Decodes accidental base64 transcript content when it is clearly text.
    fn runtime_resume_best_effort_text(content: &str) -> String {
        let trimmed = content.trim();
        Self::runtime_resume_base64_text(trimmed).unwrap_or_else(|| content.to_string())
    }

    /// Decodes one strict base64 text payload for transcript replay.
    fn runtime_resume_base64_text(content: &str) -> Option<String> {
        if content.len() < 8 || !content.len().is_multiple_of(4) {
            return None;
        }
        if !content
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(content.as_bytes())
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        if text.is_empty()
            || !text
                .chars()
                .all(|ch| matches!(ch, '\n' | '\r' | '\t') || !ch.is_control())
        {
            return None;
        }
        Some(text)
    }

    /// Extracts `structured_content.text` from replayed tool content.
    fn runtime_resume_structured_text(content: &str) -> Option<String> {
        for marker in ["structured_content: ", "structured_content="] {
            let Some((_before, after)) = content.split_once(marker) else {
                continue;
            };
            let value = serde_json::from_str::<serde_json::Value>(after.trim()).ok()?;
            if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                return Some(text.to_string());
            }
            if let Some(text) = value
                .get("structured_content")
                .and_then(|structured| structured.get("text"))
                .and_then(serde_json::Value::as_str)
            {
                return Some(text.to_string());
            }
        }
        None
    }

    /// Extracts a plain `content:` field from replayed tool content.
    fn runtime_resume_content_field_text(content: &str) -> Option<String> {
        let (_before, after) = content.split_once("content: ")?;
        let value = after
            .split(" structured_content:")
            .next()
            .unwrap_or(after)
            .trim();
        (!value.is_empty()).then(|| value.to_string())
    }

    /// Builds one bounded single-line transcript preview.
    fn runtime_resume_entry_preview(content: &str) -> String {
        let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.chars().count() <= 160 {
            return normalized;
        }
        let mut preview = normalized.chars().take(159).collect::<String>();
        preview.push('…');
        preview
    }

    /// Runs the execute agent shell fork command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_fork_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("fork command must be a slash command"))?;
        let source = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?
            .session_id
            .clone();
        let source_descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "source pane not found",
            )
        })?;
        let source_start_directory = self.pane_current_working_directory(pane_id);
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "fork".to_string(),
                body: format!(
                    "current_conversation={} forked=false reason=transcript-store-unavailable source=runtime-fork",
                    json_escape(&source)
                ),
            });
        };
        let prompt_seed =
            Self::runtime_agent_fork_prompt_seed(&store.prompt_history(&source)?, input);
        let source_lineage = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.prompt_cache_lineage_id.clone());
        let target = invocation
            .args
            .split_whitespace()
            .next()
            .map(ToOwned::to_owned)
            .unwrap_or_else(Self::runtime_new_agent_conversation_id);
        let started = self.split_pane_in_window_with_process(
            primary_client_id,
            &source_descriptor.window_id,
            SplitDirection::Vertical,
            true,
            None,
            source_start_directory.as_deref(),
        )?;
        let setup_result = (|| -> Result<_> {
            let summary = store.fork(&source, &target, current_unix_seconds().max(1))?;
            #[cfg(test)]
            if self.take_agent_fork_after_persistence_failure_for_tests() {
                return Err(MezError::invalid_state(
                    "injected agent fork failure after persistence",
                ));
            }
            self.agent_shell_store_mut()
                .enter_or_resume(&started.pane_id)?;
            let (session_id, transcript_entries, visibility) = {
                let session = self
                    .agent_shell_store_mut()
                    .bind_conversation_with_lineage(
                        &started.pane_id,
                        &summary.conversation_id,
                        summary.entries as u64,
                        source_lineage,
                    )?;
                (
                    session.session_id.clone(),
                    session.transcript_entries,
                    session.visibility,
                )
            };
            self.record_pane_transcript_ref(
                &started.pane_id,
                format!("transcript:{}:{session_id}", started.pane_id),
            )?;
            self.enter_agent_mode_for_pane(&started.pane_id)?;
            if let Some(seed) = prompt_seed
                && let Some(prompt_input) = self.agent_prompt_input_mut(&started.pane_id)
            {
                prompt_input
                    .prompt
                    .buffer
                    .apply(ReadlineEdit::InsertText(seed));
            }
            Ok((session_id, transcript_entries, visibility))
        })();
        let (session_id, transcript_entries, visibility) = match setup_result {
            Ok(result) => result,
            Err(error) => {
                let pane_cleanup = self.dispatch_runtime_pane_close(
                    primary_client_id,
                    &format!(
                        r#"{{"pane_id":"{}","force":true}}"#,
                        json_escape(&started.pane_id)
                    ),
                );
                let pane_cleanup_failed = self.find_pane_descriptor(&started.pane_id).is_some();
                let storage_cleanup = store.delete(&target);
                if pane_cleanup_failed || storage_cleanup.is_err() {
                    return Err(MezError::invalid_state(format!(
                        "agent fork setup failed: {error}; rollback failed: pane={}; storage={}",
                        pane_cleanup
                            .err()
                            .map(|cleanup| cleanup.to_string())
                            .unwrap_or_else(|| "pane still exists".to_string()),
                        storage_cleanup
                            .err()
                            .map(|cleanup| cleanup.to_string())
                            .unwrap_or_else(|| "removed".to_string())
                    )));
                }
                return Err(error);
            }
        };
        Ok(AgentShellCommandOutcome::Mutated {
            command: "fork".to_string(),
            body: format!(
                "source={} conversation_id={} entries={} source_pane={} pane={} forked=true",
                source, session_id, transcript_entries, pane_id, started.pane_id
            ),
            visibility,
        })
    }

    /// Returns the prompt text that should seed a newly forked agent pane.
    ///
    /// # Parameters
    /// - `history`: Shared persisted agent prompt history for the source
    ///   conversation.
    /// - `current_input`: The `/fork` command currently being executed.
    fn runtime_agent_fork_prompt_seed(history: &[String], current_input: &str) -> Option<String> {
        let current = current_input.trim();
        history
            .iter()
            .rev()
            .find(|entry| {
                let trimmed = entry.trim();
                !trimmed.is_empty() && (current.is_empty() || trimmed != current)
            })
            .cloned()
    }

    /// Returns a version-four UUID string for a newly forked conversation.
    pub(super) fn runtime_new_agent_conversation_id() -> String {
        let mut bytes: [u8; 16] = rand::random();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        )
    }
}
