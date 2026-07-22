//! Runtime terminal-input decoding and prompt submission adapters.
//!
//! This product boundary translates raw terminal byte sequences into mux-owned
//! overlay and selector events. It also retains prompt history, command
//! execution, and agent-shell response application; deterministic overlay and
//! selector state transitions remain in `mez_mux`.

use super::{
    AgentTerminalPresentationStyle, AgentTurnState, DEFAULT_READLINE_HISTORY_LIMIT,
    ReadlineOutcome, ReadlinePromptKind, Result, RuntimeAgentShellDisplayOutput,
    RuntimeSessionService, RuntimeSideEffect, SelectorCandidate, SelectorCandidateKind,
    SelectorExtraCandidate, SelectorSurface, TerminalClientLoopAction,
    agent_display_lines_are_error, agent_display_lines_are_low_level_status,
    agent_prompt_error_display_lines, agent_shell_mcp_display_state_name, current_unix_millis,
    default_runtime_agent_prompt_input, runtime_agent_shell_display_output,
    runtime_agent_shell_visibility, runtime_command_display_overlay_content,
    runtime_command_display_should_open_overlay,
};
use crate::runtime::service_state::RuntimeRecordBrowserOverlayState;

impl RuntimeSessionService {
    /// Runs the apply primary prompt terminal action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_primary_prompt_terminal_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: &TerminalClientLoopAction,
        queue_for_adapter: bool,
    ) -> Result<bool> {
        match action {
            TerminalClientLoopAction::ForwardToPane(input) => {
                self.apply_primary_prompt_input(primary_client_id, input, queue_for_adapter)
            }
            TerminalClientLoopAction::ForwardMouseToPane { .. }
            | TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => Ok(false),
        }
    }

    /// Runs the apply primary prompt input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn apply_primary_prompt_input(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &[u8],
        queue_for_adapter: bool,
    ) -> Result<bool> {
        if input == b"\x1b" {
            if self
                .presentation
                .primary_prompt_input
                .as_ref()
                .is_some_and(|prompt_input| prompt_input.prompt.reverse_search_active())
            {
                // Let the prompt consume Escape to cancel incremental search.
            } else {
                if self.presentation.primary_prompt_input.take().is_some() {
                    return Ok(true);
                }
                return Ok(false);
            }
        }
        if input == b"\x0c" {
            if self.presentation.primary_prompt_input.is_some() {
                let pane_id = self.active_pane_id()?;
                self.clear_agent_shell_terminal_view(&pane_id)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let selector_extra_candidates = self.runtime_command_selector_extra_candidates();
        let selector_working_directory = self
            .active_pane_id()
            .ok()
            .and_then(|pane_id| self.pane_current_working_directory(&pane_id));
        let Some(prompt_input) = self.presentation.primary_prompt_input.as_mut() else {
            return Ok(false);
        };
        if prompt_input.prompt.kind == ReadlinePromptKind::Command {
            prompt_input
                .prompt
                .set_selector_extra_candidates(selector_extra_candidates);
            prompt_input
                .prompt
                .set_selector_working_directory(selector_working_directory);
        }
        let outcomes = if input == b"\x1b" && prompt_input.prompt.reverse_search_active() {
            vec![prompt_input.prompt.apply_terminal_input(input)?]
        } else {
            prompt_input
                .decoder
                .apply_to_prompt(&mut prompt_input.prompt, input)?
        };
        let mut changed = false;
        for outcome in outcomes {
            match outcome {
                ReadlineOutcome::Submitted(command)
                | ReadlineOutcome::SubmittedWithDisplay { text: command, .. } => {
                    let prompt_kind = prompt_input.prompt.kind;
                    self.presentation.primary_prompt_input = None;
                    changed = true;
                    if !command.trim().is_empty() {
                        if prompt_kind == ReadlinePromptKind::Command {
                            self.remember_primary_command_prompt_submission(
                                &command,
                                queue_for_adapter,
                            )?;
                        }
                        match self
                            .execute_terminal_command(primary_client_id, &command)
                            .and_then(|body| {
                                runtime_command_display_overlay_content(
                                    &body,
                                    &self.presentation.settings.ui_theme,
                                    usize::from(self.session.authoritative_size.columns),
                                    self.presentation.settings.terminal_agent_wrap_column_cap,
                                )
                            }) {
                            Ok(content) => {
                                self.present_runtime_command_display_content(content)?;
                            }
                            Err(error) => {
                                self.show_primary_display_overlay(vec![format!(
                                    "error: {error} - press Esc to return"
                                )])?;
                            }
                        }
                    }
                    return Ok(changed);
                }
                ReadlineOutcome::Cancelled | ReadlineOutcome::Eof => {
                    self.presentation.primary_prompt_input = None;
                    return Ok(true);
                }
                ReadlineOutcome::Edited => changed = true,
                ReadlineOutcome::Noop => {}
            }
        }
        Ok(changed)
    }

    /// Retains one submitted `Ctrl+A :` command for future readline history
    /// navigation and reverse search.
    fn remember_primary_command_prompt_submission(
        &mut self,
        command: &str,
        queue_for_adapter: bool,
    ) -> Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }
        self.presentation
            .primary_command_prompt_history
            .push(command.to_string());
        while self.presentation.primary_command_prompt_history.len()
            > DEFAULT_READLINE_HISTORY_LIMIT
        {
            self.presentation.primary_command_prompt_history.remove(0);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(());
        };
        if queue_for_adapter {
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistCommandPromptHistory {
                    path: store.command_prompt_history_file(),
                    store,
                    command: command.to_string(),
                });
            return Ok(());
        }
        let _ = store.append_command_prompt_history(command)?;
        Ok(())
    }

    /// Reloads persisted primary command prompt history into the live prompt
    /// cache.
    pub(super) fn reload_primary_command_prompt_history(&mut self) -> Result<()> {
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(());
        };
        self.presentation.primary_command_prompt_history = store.command_prompt_history()?;
        Ok(())
    }

    /// Runs the apply attached agent prompt input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_attached_agent_prompt_input(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        if input.is_empty() {
            return Ok(false);
        }
        let pane_id = self.active_pane_id()?;
        self.apply_attached_agent_prompt_input_for_pane(primary_client_id, &pane_id, input)
    }

    /// Applies attached agent prompt input to an explicit pane.
    ///
    /// This is used by the ordinary focused-pane input path and by mouse
    /// paste routing, where the click can intentionally target a different
    /// pane-local prompt before bytes are decoded by readline.
    pub(crate) fn apply_attached_agent_prompt_input_for_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &[u8],
    ) -> Result<bool> {
        if input.is_empty() {
            return Ok(false);
        }
        if input == b"\x1b" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
        }
        if input == b"\x0c" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            self.clear_agent_shell_terminal_view(pane_id)?;
            return Ok(true);
        }
        if input != b"\x03" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
        }
        let selector_extra_candidates = self.runtime_agent_selector_extra_candidates();
        let selector_working_directory = self.pane_current_working_directory(pane_id);
        let prompt_body_columns = self
            .agent_prompt_editable_body_width(pane_id)
            .unwrap_or(1)
            .max(1);

        let outcomes = {
            let state = self
                .presentation
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state.prompt.set_prompt_body_columns(prompt_body_columns);
            state
                .prompt
                .set_selector_extra_candidates(selector_extra_candidates);
            state
                .prompt
                .set_selector_working_directory(selector_working_directory);
            if input == b"\x1b" {
                vec![state.prompt.apply_terminal_input(input)?]
            } else {
                state.decoder.apply_to_prompt(&mut state.prompt, input)?
            }
        };

        let mut changed = false;
        for outcome in outcomes {
            match outcome {
                ReadlineOutcome::Submitted(command) => {
                    changed = true;
                    if command.trim().is_empty() {
                        continue;
                    }
                    let body = match self.execute_agent_shell_command(primary_client_id, &command) {
                        Ok(body) => body,
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                            continue;
                        }
                    };
                    match runtime_agent_shell_display_output(
                        &body,
                        &self.presentation.settings.ui_theme,
                        usize::from(self.session.authoritative_size.columns),
                        self.presentation.settings.terminal_agent_wrap_column_cap,
                    ) {
                        Ok(display_output) => {
                            self.set_agent_prompt_display_output(pane_id, display_output)?;
                        }
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                        }
                    }
                    if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                        self.presentation.agent_prompt_inputs.remove(pane_id);
                    }
                }
                ReadlineOutcome::SubmittedWithDisplay { text, display } => {
                    changed = true;
                    if text.trim().is_empty() {
                        continue;
                    }
                    let body = match self.execute_agent_shell_command_with_display(
                        primary_client_id,
                        &text,
                        &display,
                    ) {
                        Ok(body) => body,
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                            continue;
                        }
                    };
                    match runtime_agent_shell_display_output(
                        &body,
                        &self.presentation.settings.ui_theme,
                        usize::from(self.session.authoritative_size.columns),
                        self.presentation.settings.terminal_agent_wrap_column_cap,
                    ) {
                        Ok(display_output) => {
                            self.set_agent_prompt_display_output(pane_id, display_output)?;
                        }
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                        }
                    }
                    if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                        self.presentation.agent_prompt_inputs.remove(pane_id);
                    }
                }
                ReadlineOutcome::Cancelled => {
                    changed = self.apply_agent_prompt_ctrl_c_interrupt_or_confirm_exit(
                        primary_client_id,
                        pane_id,
                    )?;
                }
                ReadlineOutcome::Eof => {
                    changed = true;
                    let _ = self.execute_agent_shell_command(primary_client_id, "/exit")?;
                    self.presentation.agent_prompt_inputs.remove(pane_id);
                }
                ReadlineOutcome::Edited => changed = true,
                ReadlineOutcome::Noop => {}
            }
        }
        Ok(changed)
    }

    /// Clears any pending idle Ctrl+C exit confirmation for one agent prompt.
    fn clear_agent_prompt_pending_ctrl_c_exit(&mut self, pane_id: &str) {
        if let Some(state) = self.presentation.agent_prompt_inputs.get_mut(pane_id) {
            state.pending_ctrl_c_exit_at_unix_ms = None;
        }
    }

    /// Applies the interrupt/exit contract for pane-local agent prompts.
    ///
    /// Ctrl+C confirmation and EOF exits share this helper so active work is
    /// stopped consistently before the pane-local prompt is hidden.
    fn apply_agent_prompt_interrupt_or_exit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
    ) -> Result<bool> {
        let command = if self.agent_shell_pane_has_active_turn(pane_id) {
            "/stop"
        } else {
            "/exit"
        };
        let body = self.execute_agent_shell_command(primary_client_id, command)?;
        match runtime_agent_shell_display_output(
            &body,
            &self.presentation.settings.ui_theme,
            usize::from(self.session.authoritative_size.columns),
            self.presentation.settings.terminal_agent_wrap_column_cap,
        ) {
            Ok(display_output) => self.set_agent_prompt_display_output(pane_id, display_output)?,
            Err(error) => self.set_agent_prompt_display_lines(
                pane_id,
                agent_prompt_error_display_lines(&error),
            )?,
        }
        if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
            self.presentation.agent_prompt_inputs.remove(pane_id);
        }
        Ok(true)
    }

    /// Applies the Ctrl+C interrupt or double-confirm idle exit contract.
    fn apply_agent_prompt_ctrl_c_interrupt_or_confirm_exit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
    ) -> Result<bool> {
        const CTRL_C_EXIT_CONFIRM_WINDOW_MS: u64 = 3_000;
        if self.agent_shell_pane_has_active_turn(pane_id) {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            return self.apply_agent_prompt_interrupt_or_exit(primary_client_id, pane_id);
        }

        if let Some(state) = self.presentation.agent_prompt_inputs.get_mut(pane_id)
            && !state.prompt.buffer.line().is_empty()
        {
            state.prompt.buffer.set_line("");
            state.pending_ctrl_c_exit_at_unix_ms = None;
            state.display_lines.clear();
            return Ok(true);
        }

        let now = current_unix_millis();
        let confirmed = {
            let state = self
                .presentation
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state
                .pending_ctrl_c_exit_at_unix_ms
                .is_some_and(|started| now.saturating_sub(started) <= CTRL_C_EXIT_CONFIRM_WINDOW_MS)
        };
        if confirmed {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            return self.apply_agent_prompt_interrupt_or_exit(primary_client_id, pane_id);
        }

        if let Some(state) = self.presentation.agent_prompt_inputs.get_mut(pane_id) {
            state.pending_ctrl_c_exit_at_unix_ms = Some(now);
        }
        self.set_agent_prompt_display_lines(
            pane_id,
            vec!["press ctrl-c again within 3s to exit agent mode".to_string()],
        )?;
        Ok(true)
    }

    /// Reports whether a pane-local agent shell currently owns interruptible work.
    fn agent_shell_pane_has_active_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
            || self.agent_turn_ledger().turns().iter().any(|turn| {
                turn.pane_id == pane_id
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
    }

    /// Builds dynamic primary command prompt selector candidates.
    pub(super) fn runtime_command_selector_extra_candidates(&self) -> Vec<SelectorExtraCandidate> {
        self.mcp_registry()
            .list_servers()
            .into_iter()
            .flat_map(|server| {
                let candidate = SelectorCandidate::new(
                    server.configured.id.clone(),
                    SelectorCandidateKind::Value,
                    true,
                )
                .with_detail(agent_shell_mcp_display_state_name(
                    server.configured.enabled,
                    server.status,
                ));
                [
                    SelectorExtraCandidate::new(
                        SelectorSurface::MezzanineCommand,
                        "mcp-status",
                        candidate.clone(),
                    ),
                    SelectorExtraCandidate::new(
                        SelectorSurface::MezzanineCommand,
                        "mcp inspect",
                        candidate,
                    ),
                ]
            })
            .collect()
    }

    /// Builds dynamic agent prompt selector candidates from saved transcripts.
    fn runtime_agent_selector_extra_candidates(&self) -> Vec<SelectorExtraCandidate> {
        let mut candidates = self
            .integration
            .agent_personality_profiles()
            .iter()
            .map(|(profile_id, profile)| {
                SelectorExtraCandidate::new(
                    SelectorSurface::AgentCommand,
                    "personality",
                    SelectorCandidate::new(profile_id.clone(), SelectorCandidateKind::Value, true)
                        .with_detail(
                            profile
                                .name
                                .clone()
                                .unwrap_or_else(|| "personality profile".to_string()),
                        ),
                )
            })
            .collect::<Vec<_>>();
        candidates.extend(
            self.mcp_registry()
                .list_servers()
                .into_iter()
                .flat_map(|server| {
                    let detail = agent_shell_mcp_display_state_name(
                        server.configured.enabled,
                        server.status,
                    );
                    let list_candidate = SelectorCandidate::new(
                        server.configured.id.clone(),
                        SelectorCandidateKind::Value,
                        true,
                    )
                    .with_detail(detail);
                    let prompt_candidate = SelectorCandidate::new(
                        format!("@{}", server.configured.id),
                        SelectorCandidateKind::Value,
                        true,
                    )
                    .with_detail(detail);
                    [
                        SelectorExtraCandidate::new(
                            SelectorSurface::AgentCommand,
                            "list-mcp",
                            list_candidate,
                        ),
                        SelectorExtraCandidate::new(
                            SelectorSurface::AgentCommand,
                            "@",
                            prompt_candidate,
                        ),
                    ]
                }),
        );
        if let Ok(pane_id) = self.active_pane_id() {
            let catalog = self.effective_skill_catalog_for_pane(&pane_id);
            candidates.extend(catalog.skills.into_iter().map(|skill| {
                SelectorExtraCandidate::new(
                    SelectorSurface::AgentCommand,
                    "$",
                    SelectorCandidate::new(
                        format!("${}", skill.name),
                        SelectorCandidateKind::Value,
                        true,
                    )
                    .with_detail(format!(
                        "{} ({})",
                        skill.description,
                        skill.source.as_str()
                    )),
                )
            }));
            let macro_catalog = self.effective_macro_catalog_for_pane(&pane_id);
            candidates.extend(macro_catalog.macros.into_iter().map(|macro_summary| {
                SelectorExtraCandidate::new(
                    SelectorSurface::AgentCommand,
                    "#",
                    SelectorCandidate::new(
                        format!("#{}", macro_summary.name),
                        SelectorCandidateKind::Value,
                        true,
                    )
                    .with_detail(format!(
                        "{} ({}; {} steps)",
                        macro_summary.description,
                        macro_summary.source.as_str(),
                        macro_summary.step_count
                    )),
                )
            }));
        }
        if crate::runtime::commands::runtime_issues_enabled(self)
            && let Some(config_root) = self.integration.config_root()
        {
            let store = crate::storage::issues::IssueStore::from_database_path(
                crate::runtime::commands::runtime_issue_database_path(
                    self,
                    &config_root.to_path_buf(),
                ),
            );
            candidates.extend(
                store
                    .list_issue_projects()
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(|project| {
                        ["--project", "--project-glob"]
                            .into_iter()
                            .map(move |option| {
                                SelectorExtraCandidate::after_option(
                                    SelectorSurface::AgentCommand,
                                    "show-issues",
                                    option,
                                    SelectorCandidate::new(
                                        project.clone(),
                                        SelectorCandidateKind::Value,
                                        true,
                                    ),
                                )
                            })
                    }),
            );
        }
        let Some(store) = self.persistence.transcript_store() else {
            return candidates;
        };
        candidates.extend(store.list().unwrap_or_default().into_iter().map(|summary| {
            SelectorExtraCandidate::new(
                SelectorSurface::AgentCommand,
                "resume",
                SelectorCandidate::new(
                    summary.conversation_id.clone(),
                    SelectorCandidateKind::Value,
                    true,
                )
                .with_detail(format!(
                    "{} entries, pane {}, agent {}",
                    summary.entries, summary.pane_id, summary.agent_id
                )),
            )
        }));
        candidates
    }

    /// Runs the reload agent prompt history for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn reload_agent_prompt_history_for_pane(&mut self, pane_id: &str) -> Result<()> {
        let Some(session_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let history = match self.persistence.transcript_store() {
            Some(store) => match store.prompt_history(&session_id) {
                Ok(history) => history,
                Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error),
            },
            None => Vec::new(),
        };
        self.presentation
            .agent_prompt_inputs
            .entry(pane_id.to_string())
            .or_insert_with(default_runtime_agent_prompt_input)
            .prompt
            .buffer
            .set_history(history);
        Ok(())
    }

    /// Runs the set agent prompt display lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn set_agent_prompt_display_lines(
        &mut self,
        pane_id: &str,
        display_lines: Vec<String>,
    ) -> Result<()> {
        let style = if agent_display_lines_are_error(&display_lines) {
            AgentTerminalPresentationStyle::Error
        } else {
            AgentTerminalPresentationStyle::Assistant
        };
        if style == AgentTerminalPresentationStyle::Error
            || self.agent_verbose_enabled(pane_id)
            || !agent_display_lines_are_low_level_status(&display_lines)
        {
            self.append_agent_terminal_lines_to_buffer(pane_id, &display_lines, style)?;
        }
        let state = self
            .presentation
            .agent_prompt_inputs
            .entry(pane_id.to_string())
            .or_insert_with(default_runtime_agent_prompt_input);
        state.display_lines.clear();
        Ok(())
    }

    /// Appends agent shell display output using the declared content renderer.
    pub(super) fn set_agent_prompt_display_output(
        &mut self,
        pane_id: &str,
        display_output: RuntimeAgentShellDisplayOutput,
    ) -> Result<()> {
        match display_output {
            RuntimeAgentShellDisplayOutput::Suppressed => {
                let state = self
                    .presentation
                    .agent_prompt_inputs
                    .entry(pane_id.to_string())
                    .or_insert_with(default_runtime_agent_prompt_input);
                state.display_lines.clear();
            }
            RuntimeAgentShellDisplayOutput::TransientStatus(display_lines) => {
                self.show_primary_notice_overlay(display_lines)?;
                let state = self
                    .presentation
                    .agent_prompt_inputs
                    .entry(pane_id.to_string())
                    .or_insert_with(default_runtime_agent_prompt_input);
                state.display_lines.clear();
            }
            RuntimeAgentShellDisplayOutput::Lines(display_lines) => {
                self.set_agent_prompt_display_lines(pane_id, display_lines)?;
            }
            RuntimeAgentShellDisplayOutput::Overlay(content) => {
                let should_open_overlay = runtime_command_display_should_open_overlay(&content);
                let record_browser = content.command.as_ref().and_then(|command| {
                    let key = (pane_id.to_string(), command.clone());
                    let source = self
                        .presentation
                        .pending_record_browser_overlay_sources
                        .remove(&key);
                    let stack = self
                        .presentation
                        .pending_record_browser_overlay_stacks
                        .remove(&key)
                        .unwrap_or_default();
                    self.presentation
                        .pending_record_browser_overlays
                        .remove(&key)
                        .map(|browser| RuntimeRecordBrowserOverlayState {
                            pane_id: pane_id.to_string(),
                            command: command.clone(),
                            source,
                            browser,
                            stack,
                        })
                });
                if should_open_overlay {
                    self.show_primary_display_overlay_inner(
                        content.lines,
                        content.line_style_spans,
                        content.line_copy_texts,
                        content.selections,
                        false,
                    )?;
                    if let (Some(overlay), Some(record_browser)) = (
                        self.presentation.primary_display_overlay.as_mut(),
                        record_browser,
                    ) {
                        overlay.record_browser = Some(record_browser);
                    }
                } else {
                    self.set_agent_prompt_display_lines(pane_id, content.lines)?;
                }
                let state = self
                    .presentation
                    .agent_prompt_inputs
                    .entry(pane_id.to_string())
                    .or_insert_with(default_runtime_agent_prompt_input);
                state.display_lines.clear();
            }
        }
        Ok(())
    }

    /// Presents one encoded agent-shell display response through the same
    /// renderer path used by live terminal input.
    #[cfg(test)]
    pub(crate) fn set_agent_prompt_response_display_output_for_tests(
        &mut self,
        pane_id: &str,
        response: &str,
    ) -> Result<()> {
        let display_output = runtime_agent_shell_display_output(
            response,
            &self.presentation.settings.ui_theme,
            usize::from(self.session.authoritative_size.columns),
            self.presentation.settings.terminal_agent_wrap_column_cap,
        )?;
        self.set_agent_prompt_display_output(pane_id, display_output)
    }
}
