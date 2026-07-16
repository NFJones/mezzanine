//! Agent-shell saved-session resume command helpers.
//!
//! This module keeps `/resume` and `/list-sessions` transcript replay logic out
//! of the broader command dispatcher. It owns only saved-session selection,
//! directory restoration, transcript display fallback formatting, and the
//! related markdown list rendering used by the agent shell.

use super::{
    AgentShellCommandOutcome, MezError, PathBuf, Result, RuntimeSessionService, SplitDirection,
    TranscriptEntry, TranscriptRole, current_unix_seconds, json_escape, parse_slash_command,
    runtime_fit_status_line, runtime_markdown_table, session_state_name, shell_command_from_argv,
    unix_seconds_to_rfc3339,
};
use base64::Engine;
use mez_agent::transcript::ConversationSummary;
use mez_mux::readline::ReadlineEdit;

/// Maximum saved transcript entries to render when `/resume` has no presentation log.
const AGENT_RESUME_TRANSCRIPT_REPLAY_ENTRIES: usize = 64;
/// Maximum transcript bytes to read for `/resume` fallback replay.
const AGENT_RESUME_TRANSCRIPT_REPLAY_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum persisted presentation rows to replay when resuming an agent shell.
const AGENT_RESUME_PRESENTATION_REPLAY_ENTRIES: usize = 200;
/// Maximum cleartext presentation bytes to read when resuming an agent shell.
const AGENT_RESUME_PRESENTATION_REPLAY_BYTES: u64 = 2 * 1024 * 1024;

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

impl RuntimeSessionService {
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
            return self.execute_agent_shell_list_sessions_command(pane_id);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "resume".to_string(),
                body: "conversations=0 source=unavailable".to_string(),
            });
        };
        let conversation_id = match conversation_arg {
            Some("--latest" | "latest") => {
                let summaries = store.list()?;
                let Some(conversation_id) = Self::runtime_latest_agent_saved_session_id(&summaries)
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
            None => unreachable!("bare resume returns through list-sessions before store lookup"),
        };
        let summary = store.summary(&conversation_id)?.ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "conversation transcript not found",
            )
        })?;
        let entries = store.inspect_recent(
            &conversation_id,
            AGENT_RESUME_TRANSCRIPT_REPLAY_ENTRIES,
            AGENT_RESUME_TRANSCRIPT_REPLAY_BYTES,
        )?;
        let presentation_entries = store.inspect_recent_presentation(
            &conversation_id,
            AGENT_RESUME_PRESENTATION_REPLAY_ENTRIES,
            AGENT_RESUME_PRESENTATION_REPLAY_BYTES,
        )?;
        let resume_directory = runtime_resume_directory_from_summary(&summary)
            .or_else(|| runtime_resume_directory_from_entries(&entries));
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
        self.restore_agent_resume_directory(pane_id, resume_directory.as_deref())?;
        self.restore_agent_resume_state_for_conversation(pane_id, &session_id)?;
        self.record_pane_transcript_ref(pane_id, format!("transcript:{pane_id}:{session_id}"))?;
        self.reload_agent_prompt_history_for_pane(pane_id)?;
        self.clear_agent_shell_terminal_view(pane_id)?;
        if !self
            .replay_agent_presentation_entries_to_terminal_buffer(pane_id, &presentation_entries)?
        {
            self.set_agent_prompt_display_lines(
                pane_id,
                Self::runtime_resume_transcript_display(&summary, &entries),
            )?;
        }
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
    fn runtime_latest_agent_saved_session_id(
        summaries: &[mez_agent::transcript::ConversationSummary],
    ) -> Option<String> {
        let mut sorted_summaries = summaries.iter().collect::<Vec<_>>();
        sorted_summaries.sort_by(|left, right| {
            right
                .last_created_at_unix_seconds
                .cmp(&left.last_created_at_unix_seconds)
                .then_with(|| {
                    right
                        .first_created_at_unix_seconds
                        .cmp(&left.first_created_at_unix_seconds)
                })
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        sorted_summaries
            .first()
            .map(|summary| summary.conversation_id.clone())
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

    /// The attached prompt path owns pane-buffer rendering for display outcomes.
    /// Keeping this command side-effect-free prevents duplicate command output
    /// when the response body is rendered as markdown.
    pub(super) fn execute_agent_shell_list_sessions_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let body = self.runtime_agent_list_sessions_display(pane_id)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "list-sessions".to_string(),
            body,
        })
    }

    /// Builds `/list-sessions` from saved agent conversation transcripts.
    fn runtime_agent_list_sessions_display(&self, pane_id: &str) -> Result<String> {
        let width = self
            .pane_screen(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .unwrap_or(120);
        if let Some(store) = self.persistence.transcript_store() {
            return Ok(Self::runtime_agent_saved_sessions_display(
                &store.list()?,
                width,
            ));
        }
        Ok(self.runtime_current_session_display())
    }

    /// Formats the active in-memory session for `/list-sessions` fallback output.
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

    /// Encodes one agent command as a markdown link destination.
    fn markdown_link_destination(command: &str) -> String {
        let mut encoded = String::from("mez-agent:");
        for byte in command.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    encoded.push(char::from(byte))
                }
                other => encoded.push_str(&format!("%{other:02X}")),
            }
        }
        encoded
    }

    /// Formats saved agent conversations as a nested resume list.
    fn runtime_agent_saved_sessions_display(
        summaries: &[mez_agent::transcript::ConversationSummary],
        width: usize,
    ) -> String {
        let mut lines = vec!["## Agent Sessions".to_string(), String::new()];
        if summaries.is_empty() {
            lines.push("No saved agent sessions are available.".to_string());
            return lines.join("\n");
        }
        let mut sorted_summaries = summaries.iter().collect::<Vec<_>>();
        sorted_summaries.sort_by(|left, right| {
            right
                .last_created_at_unix_seconds
                .cmp(&left.last_created_at_unix_seconds)
                .then_with(|| {
                    right
                        .first_created_at_unix_seconds
                        .cmp(&left.first_created_at_unix_seconds)
                })
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        for (index, summary) in sorted_summaries.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            let resume_command = format!("/resume {}", summary.conversation_id);
            lines.push(format!(
                "- [**{}**]({})",
                summary.conversation_id,
                Self::markdown_link_destination(&resume_command)
            ));
            lines.push(format!(
                "  - Last Active: {}",
                unix_seconds_to_rfc3339(summary.last_created_at_unix_seconds)
            ));
            lines.push(format!(
                "  - Directory: {}",
                summary.directory.as_deref().unwrap_or("-")
            ));
            lines.push(format!(
                "  - Prompt: {}",
                runtime_fit_status_line(
                    summary.latest_user_prompt.as_deref().unwrap_or("-"),
                    width.saturating_sub("  - Prompt: ".len())
                )
            ));
        }
        lines.join("\n")
    }

    /// Formats a resumed transcript as prompt display lines so the user can
    /// pick up the saved conversation with visible context in the pane.
    fn runtime_resume_transcript_display(
        summary: &ConversationSummary,
        entries: &[TranscriptEntry],
    ) -> Vec<String> {
        let mut lines = vec!["Resumed Agent Session".to_string()];
        if entries.is_empty() {
            lines.push("No saved transcript entries were found.".to_string());
            return lines;
        }
        lines.push(format!(
            "Conversation ID: {} | Entries: {} | Resumed: yes",
            json_escape(&summary.conversation_id),
            summary.entries
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
        let summary = store.fork(&source, &target, current_unix_seconds().max(1))?;
        let started = self.split_pane_in_window_with_process(
            primary_client_id,
            &source_descriptor.window_id,
            SplitDirection::Vertical,
            true,
            None,
            source_start_directory.as_deref(),
        )?;
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
