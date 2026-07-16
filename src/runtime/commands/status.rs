//! Agent status, title, status-line, and debug display commands.
//!
//! This module owns read-mostly agent presentation commands and their display
//! formatting helpers: `/statusline`, `/title`, `/status`, terminal-view
//! clearing, and `/debug-config`. Keeping these report builders outside the
//! command facade separates UI/status presentation from turn orchestration and
//! policy mutation.

use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentShellCommandOutcome, BTreeMap,
    ConfigFormat, ConfigScope, MezError, ModelTokenUsage, ModelTokenUsageKey, Result,
    RuntimeSessionService, agent_shell_visibility_json_name, compose_effective_config,
    execute_command, json_escape, parse_slash_command, runtime_agent_turn_state_name,
    runtime_approval_policy_name, runtime_cooperation_mode_name, runtime_markdown_table,
    runtime_permission_preset_name, runtime_single_rename_window_invocation,
    runtime_statusline_fields, runtime_statusline_template, runtime_string_array_json,
};

impl RuntimeSessionService {
    /// Executes `/statusline` against live pane frame status-line settings.
    pub(super) fn execute_agent_shell_statusline_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("statusline command must be a slash command"))?;
        if invocation.args.trim().is_empty() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "statusline".to_string(),
                body: self.runtime_agent_statusline_display(),
            });
        }
        let fields = runtime_statusline_fields(&invocation.args)?;
        self.configure_pane_statusline(fields.clone(), runtime_statusline_template(&fields));
        Ok(AgentShellCommandOutcome::Mutated {
            command: "statusline".to_string(),
            body: format!("{} changed=true", self.runtime_agent_statusline_display()),
            visibility,
        })
    }

    /// Builds the live `/statusline` display from pane frame status settings.
    pub(super) fn runtime_agent_statusline_display(&self) -> String {
        format!(
            "enabled={} fields={} template={} source=runtime-statusline",
            self.pane_frames_enabled(),
            runtime_string_array_json(self.pane_frame_visible_fields()),
            json_escape(self.pane_frame_template())
        )
    }

    /// Executes `/title` against the active runtime window title.
    pub(super) fn execute_agent_shell_title_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("title command must be a slash command"))?;
        if slash.args.trim().is_empty() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "title".to_string(),
                body: self.runtime_agent_title_display(pane_id)?,
            });
        }
        let invocation = runtime_single_rename_window_invocation(&slash.args)?;
        execute_command(&mut self.session, primary_client_id, &invocation)?;
        let body = format!(
            "{} changed=true",
            self.runtime_agent_title_display(pane_id)?
        );
        Ok(AgentShellCommandOutcome::Mutated {
            command: "title".to_string(),
            body,
            visibility,
        })
    }

    /// Builds the live `/title` display for the active window and pane.
    pub(super) fn runtime_agent_title_display(&self, pane_id: &str) -> Result<String> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane_title = self
            .find_pane_title(pane_id)
            .unwrap_or_else(|| "unknown".to_string());
        Ok(format!(
            "window_id={} window_title={} pane={} pane_title={} source=runtime-title",
            json_escape(window.id.as_str()),
            json_escape(&window.title()),
            json_escape(pane_id),
            json_escape(&pane_title)
        ))
    }

    /// Executes `/status` against the live runtime status source.
    pub(super) fn execute_agent_shell_status_command(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        Ok(AgentShellCommandOutcome::Display {
            command: "status".to_string(),
            body: self.runtime_agent_status_display(pane_id)?,
        })
    }

    /// Builds the live `/status` display from runtime session state.
    pub(super) fn runtime_agent_status_display(&self, pane_id: &str) -> Result<String> {
        let session = self.agent_shell_store().get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent shell session not found for pane",
            )
        })?;
        let agent_id = format!("agent-{pane_id}");
        let descriptor = self.find_pane_descriptor(pane_id);
        let window_id = descriptor
            .as_ref()
            .map(|descriptor| descriptor.window_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let current_working_directory = self
            .pane_current_working_directory(pane_id)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_scopes = self.active_subagent_write_scopes_for(&agent_id);
        let writable_roots = active_scopes
            .iter()
            .map(|scope| scope.scope.clone())
            .collect::<Vec<_>>();
        let latest_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.pane_id == pane_id);
        let latest_turn_id = latest_turn
            .map(|turn| turn.turn_id.as_str())
            .unwrap_or("none");
        let latest_turn_state = latest_turn
            .map(|turn| runtime_agent_turn_state_name(turn.state))
            .unwrap_or("none");
        let context_blocks = latest_turn
            .and_then(|turn| self.agent_turn_contexts().get(&turn.turn_id))
            .map(|context| context.blocks.len())
            .unwrap_or(0);
        let request_messages = latest_turn
            .and_then(|turn| self.agent_turn_executions().get(&turn.turn_id))
            .map(|execution| execution.request.messages.len())
            .unwrap_or(0);
        let token_usage_by_model = self.agent_token_usage_for_pane(pane_id);
        let instance_token_usage_by_model =
            self.runtime_agent_instance_provider_token_usage_by_model();
        let running_turn = session
            .running_turn_id
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let reasoning_profile = model_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let thinking = self
            .model_profile_thinking_enabled(&model_profile)
            .map(|enabled| if enabled { "enabled" } else { "disabled" })
            .unwrap_or("unsupported");
        let rows = vec![
            vec!["Pane".to_string(), session.pane_id.clone()],
            vec!["Session".to_string(), session.session_id.clone()],
            vec![
                "Visibility".to_string(),
                agent_shell_visibility_json_name(session.visibility).to_string(),
            ],
            vec!["Running turn".to_string(), running_turn],
            vec![
                "Transcript entries".to_string(),
                session.transcript_entries.to_string(),
            ],
            vec![
                "Directive".to_string(),
                session
                    .directive
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ],
            vec![
                "Log level".to_string(),
                session.log_level.as_str().to_string(),
            ],
            vec!["Agent id".to_string(), agent_id],
            vec!["Window id".to_string(), window_id],
            vec!["Current directory".to_string(), current_working_directory],
            vec![
                "Model".to_string(),
                format!(
                    "{} via {} (profile: {}, reasoning: {})",
                    model_profile.model,
                    model_profile.provider,
                    model_profile_name,
                    reasoning_profile
                ),
            ],
            vec!["Thinking".to_string(), thinking.to_string()],
            vec![
                "Prompt profile".to_string(),
                format!("{AGENT_PROMPT_PROFILE_NAME} v{AGENT_PROMPT_PROFILE_VERSION}"),
            ],
            vec![
                "Permissions".to_string(),
                format!(
                    "preset {}, approval {}, bypass {}",
                    runtime_permission_preset_name(self.permission_policy().preset),
                    runtime_approval_policy_name(self.permission_policy().approval_policy),
                    self.permission_policy().approval_bypass()
                ),
            ],
            vec![
                "Command rules".to_string(),
                self.permission_policy().rules().len().to_string(),
            ],
            vec![
                "Writable roots".to_string(),
                format!(
                    "{} ({})",
                    if writable_roots.is_empty() {
                        "none".to_string()
                    } else {
                        writable_roots.join(", ")
                    },
                    writable_roots.len()
                ),
            ],
            vec![
                "Active write scopes".to_string(),
                self.active_subagent_write_scope_count().to_string(),
            ],
            vec![
                "Context".to_string(),
                format!(
                    "{context_blocks} blocks, {request_messages} request messages, window={} tokens, compaction=provider-rejection/manual",
                    model_profile.context_window_tokens()
                ),
            ],
            vec![
                "Pane agent tokens".to_string(),
                Self::runtime_agent_provider_token_usage_summary(&token_usage_by_model),
            ],
            vec![
                "Latest turn".to_string(),
                format!("{latest_turn_id} ({latest_turn_state})"),
            ],
        ];
        let mut lines = vec!["## Agent Status".to_string(), String::new()];
        lines.extend(runtime_markdown_table(&["Field", "Value"], &rows));
        if !token_usage_by_model.is_empty() {
            lines.push(String::new());
            lines.push("### Pane Agent Token Usage".to_string());
            lines.push(String::new());
            lines.extend(runtime_markdown_table(
                &[
                    "Provider",
                    "Model",
                    "Billed input",
                    "Cached input",
                    "Output",
                    "Reasoning",
                    "Cache Hit %",
                ],
                &Self::runtime_agent_provider_token_usage_rows(&token_usage_by_model),
            ));
        }
        if !instance_token_usage_by_model.is_empty() {
            lines.push(String::new());
            lines.push("### Mez Session Token Usage".to_string());
            lines.push(String::new());
            lines.extend(runtime_markdown_table(
                &[
                    "Provider",
                    "Model",
                    "Billed input",
                    "Cached input",
                    "Output",
                    "Reasoning",
                    "Cache Hit %",
                ],
                &Self::runtime_agent_provider_token_usage_rows(&instance_token_usage_by_model),
            ));
        }
        if !active_scopes.is_empty() {
            let scope_rows = active_scopes
                .into_iter()
                .map(|scope| {
                    vec![
                        scope.scope,
                        scope.agent_id,
                        runtime_cooperation_mode_name(scope.mode).to_string(),
                        scope.serial_lock.unwrap_or_else(|| "none".to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            lines.push(String::new());
            lines.push("### Writable Roots".to_string());
            lines.extend(runtime_markdown_table(
                &["Root", "Owner", "Mode", "Serial lock"],
                &scope_rows,
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Returns the compact `/status` summary for per-model provider tokens.
    fn runtime_agent_provider_token_usage_summary(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> String {
        match usage_by_model.len() {
            0 => "none".to_string(),
            1 => usage_by_model
                .iter()
                .next()
                .map(|(key, usage)| {
                    format!(
                        "{}: {}",
                        key.display_name(),
                        Self::runtime_agent_provider_token_usage_metrics(*usage)
                    )
                })
                .unwrap_or_else(|| "none".to_string()),
            count => format!("{count} models; see Pane Agent Token Usage"),
        }
    }

    /// Aggregates provider/model token accounting across retained conversations.
    fn runtime_agent_instance_provider_token_usage_by_model(
        &self,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.total_agent_token_usage_by_model()
    }

    /// Builds markdown table rows for per-model provider token accounting.
    fn runtime_agent_provider_token_usage_rows(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> Vec<Vec<String>> {
        usage_by_model
            .iter()
            .map(|(key, usage)| {
                vec![
                    key.provider.clone(),
                    key.model.clone(),
                    usage.billed_input_tokens().to_string(),
                    usage.cached_input_tokens_display(),
                    usage.output_tokens.to_string(),
                    usage.reasoning_tokens.to_string(),
                    usage.cached_input_hit_ratio_display(),
                ]
            })
            .collect()
    }

    /// Formats one provider/model token usage value for compact displays.
    fn runtime_agent_provider_token_usage_metrics(usage: ModelTokenUsage) -> String {
        format!(
            "input={} cached_input={} cache_hit={} output={} reasoning={} total={}",
            usage.billed_input_tokens(),
            usage.cached_input_tokens_display(),
            usage.cached_input_hit_ratio_display(),
            usage.output_tokens,
            usage.reasoning_tokens,
            usage.total_tokens()
        )
    }

    /// Moves the current terminal view into history and clears the viewport.
    pub(crate) fn clear_agent_shell_terminal_view(&mut self, pane_id: &str) -> Result<bool> {
        self.active_copy_modes_mut().remove(pane_id);
        let Some(screen) = self.pane_screen_mut(pane_id) else {
            return Ok(false);
        };
        screen.clear_visible_into_history();
        Ok(true)
    }

    /// Runs the execute agent shell debug config command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_debug_config_command(
        &self,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("debug-config command must be a slash command")
        })?;
        let filter = invocation.args.split_whitespace().next();
        Ok(AgentShellCommandOutcome::Display {
            command: "debug-config".to_string(),
            body: self.runtime_debug_config_display(filter)?,
        })
    }

    /// Builds the live `/debug-config` display from effective runtime config state.
    pub(super) fn runtime_debug_config_display(&self, filter: Option<&str>) -> Result<String> {
        let effective = compose_effective_config(self.integration.config_layers())?;
        let mut lines = vec![format!(
            "layers={} applied_layers={} skipped_layers={} values={} diagnostics={} permission_preset={} approval_policy={} bypass={} providers={} model_profiles={} mcp_servers={} hooks={} source=runtime-config",
            self.integration.config_layers().len(),
            effective.applied_layers().len(),
            effective.skipped_layers().len(),
            effective.values().len(),
            effective.diagnostics().len(),
            runtime_permission_preset_name(self.permission_policy().preset),
            runtime_approval_policy_name(self.permission_policy().approval_policy),
            self.permission_policy().approval_bypass(),
            self.provider_registry().providers.len(),
            self.provider_registry().profiles.len(),
            self.mcp_registry().list_servers().len(),
            self.integration.hook_definitions().len()
        )];
        for (index, layer) in self.integration.config_layers().iter().enumerate() {
            lines.push(format!(
                "layer={} index={} scope={} trusted={} applied={} skipped={} format={} path={}",
                json_escape(&layer.name),
                index,
                Self::runtime_config_scope_name(layer.scope),
                layer.trusted,
                effective.applied_layers().contains(&layer.name),
                effective.skipped_layers().contains(&layer.name),
                Self::runtime_config_format_name(layer.format),
                layer
                    .path
                    .as_ref()
                    .map(|path| json_escape(&path.to_string_lossy()))
                    .unwrap_or_else(|| "inline".to_string())
            ));
        }
        for diagnostic in effective.diagnostics() {
            lines.push(format!(
                "diagnostic path={} message={}",
                json_escape(&diagnostic.path),
                json_escape(&diagnostic.message)
            ));
        }
        for (path, value) in effective.values() {
            if filter.is_some_and(|filter| filter != path) {
                continue;
            }
            lines.push(format!(
                "value path={} source={} value={}",
                json_escape(path),
                json_escape(&value.source_layer),
                json_escape(&value.value)
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Runs the runtime config scope name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_scope_name(scope: ConfigScope) -> &'static str {
        match scope {
            ConfigScope::Primary => "primary",
            ConfigScope::ProjectOverlay => "project-overlay",
            ConfigScope::LiveOverride => "live-override",
        }
    }

    /// Runs the runtime config format name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_format_name(format: ConfigFormat) -> &'static str {
        match format {
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
            ConfigFormat::Json => "json",
        }
    }
}
